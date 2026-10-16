// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Located console attention within the resident brain. Input is intent;
//! only coherent displayed occupancy licenses contact and placement.

use super::*;
use crate::pet_world::TaskbarProgress;
use crate::pet_world::{PetAnchor, PetPane, PetRect, PetWorld, PetWorldFacts, PetWorldStamp};
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
/// The caret is home. Located output may direct the gaze, but must not park
/// the resident across the pane. Rows cost twice a column in this bound.
const HOME_REACH: f32 = 6.0;

/// How long the resident holds still for an unpainted caret before the
/// ordinary ladder takes over again. A DECTCEM hide means "I am repainting",
/// which takes milliseconds, so a quarter second is many repaints' worth of
/// patience. Past it the caret is still known (the host feeds it hidden or
/// not), so the pet follows it rather than sitting through a long build or
/// pager with its cursor hidden.
const HIDDEN_HOLD_MAX: f32 = 0.25;

/// How long a spent interest (a content perch with no live event left) may
/// hold the resident before the pet gets its own life back. Without it the
/// console layer would answer `console_frames() == Some(false)` and
/// `console_deadline() == None` forever, and the pet could never breathe,
/// blink, settle, sleep or notice the caret leaving.
const PERCH_DWELL: f32 = 3.0;

/// The visibility hold's dwell, in both directions. Per-frame presence from
/// screen content can reverse every ~158 ms during typing; a shorter hold
/// only slows that strobe, so this is 1.6x longer. It is also the budget the
/// pet has to walk out of the way before it must fade instead.
const VETO_DWELL: f32 = 0.25;
/// When an obstruction is real and sustained the pet yields over a ramp, not a
/// hard cut: a fifth of a second reads as the cat stepping aside without
/// painting over the user's text for long. The ramp back in is normally the
/// pet's own 0.30 s arrival fade, because a body hidden long enough to be
/// reseated returns at a new station (`reseat_unshown_console_body`), which
/// retires the held verdict.
const VETO_FADE: f32 = 0.20;
/// An unbroken run of unknown verdicts this long (a surface that never
/// regains coherence) counts as obstruction, so a retired world cannot strand
/// an opaque pet on glass. Counts only unknowns about a body being emitted; a
/// frame with no body goes to [`VisibilityHold::idle`].
const VETO_BLIND: f32 = 0.50;
/// Coming back IN FRONT of the glyphs waits this long with the drawn body
/// clear of ink. [`VETO_DWELL`]'s reasoning — longer than a keystroke's
/// half-period, so a body crossing the last typed glyph on every key cannot
/// strobe — and its value, so the two perception holds cannot drift apart.
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

/// The pet's presence as a held state rather than a per-frame boolean.
///
/// Fed the three-valued clearance verdict for the emitted sprite rectangle, it
/// answers a 0..=1 cover factor:
///
///  * an `Unknown` verdict neither flips the state nor resets a run in
///    progress;
///  * contrary evidence must persist for [`VETO_DWELL`] before the held
///    verdict changes, and the cover then ramps over [`VETO_FADE`].
///
/// So a single obstructed frame cannot blank the pet, and a single clear
/// frame cannot snap it back.
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
    /// since the dwell discounts perception wobble. Pinned by
    /// `full_selection_still_protects_the_console_and_explains_the_hidden_body`.
    by_fiat: bool,
}

impl Default for VisibilityHold {
    fn default() -> Self {
        // Shown and opaque: the pet's OWN arrival fade owns the ramp-in, and
        // a hold that started hidden would fight it.
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

    /// Nothing was drawn at all, which is a different `None` from "I cannot
    /// see there". The emitter often produces no body (an arrival fade not yet
    /// started, a finished retirement, no caret); with no rectangle there is
    /// nothing to be blind about, so this advances the ramp without touching
    /// any run, and in particular never feeds [`VETO_BLIND`].
    fn idle(&mut self, now: Instant) -> f32 {
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

    /// Put the envelope where the glass already is: hidden and settled, with
    /// no run in flight. For the paths that yield AT ONCE by design.
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

/// The emitted Z-ORDER (`PetFrame::under_ink`) as a HELD STATE.
///
/// Recomputed per frame it flipped at the keystroke rate — 50 flips over
/// one 80 ms/key pangram — and every flip swaps the cat between beneath and
/// above the glyphs, the ribbon light, the caret halo and the sparkles.
/// The two directions are not symmetric, on purpose: going UNDER is
/// immediate, because a body is never painted in front of the user's text;
/// coming back OVER needs the body clear of ink for [`Z_DWELL`]. A frame
/// that draws nothing releases the hold — a flip is only visible between
/// two drawn frames, so the next drawn one reads the ground afresh.
#[derive(Clone, Copy, Debug, Default)]
struct ZHold {
    under: bool,
    /// Start of the current unbroken run of clear frames spent `under`.
    clear_since: Option<Instant>,
}

impl ZHold {
    /// Advance one DRAWN frame; `ink` is real ink under the drawn body.
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

/// Fold the hold's cover into an emitted opacity byte. Floored at 1 while
/// any cover remains, for the reason the arrival ramp is floored: hosts gate
/// "is the pet on glass" on `alpha > 0`, and rounding a faint-but-present
/// body to zero would drop the sprite, its hit box and its motes for a frame
/// — the exact cut this envelope exists to remove.
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
struct PerchTrip {
    from: (f32, f32),
    to: (f32, f32),
    elapsed: f32,
    duration: f32,
    hop: bool,
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
    target: Option<PetRect>,
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
    repair_target: Option<(f32, f32)>,
    progress_key: Option<u64>,
    progress: Option<TaskbarProgress>,
    progress_at: Option<Instant>,
    trip: Option<PerchTrip>,
    lift: f32,
    source_seq: u64,
    completion_seq: u64,
    failure_quiet_until: Option<Instant>,
    last_body: Option<PetRect>,
    /// The brain's own `(col, row)` when [`Self::last_body`] was drawn: what
    /// the no-caret hold re-seats from. The drawn rect is not a seat — it
    /// carries the pose's lift and the row-0 pixel clamp.
    last_seat: Option<(f32, f32)>,
    /// THE ONE EXPLICIT FACT the reseat needs: did the last emission actually
    /// put pixels on glass? This used to be inferred from `last_body` being
    /// `None`, which the visibility veto ALSO wrote — so a vetoed frame read
    /// as "never drawn", teleported the pet and restarted its fade from zero.
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
}

impl ConsoleLife {
    /// A fresh console life for a retired coordinate space. It keeps only the
    /// world's buffer, retired exactly as a default map, as the spare the
    /// next observation fills.
    pub(super) fn retired(&mut self) -> Self {
        let spare_world = self
            .world
            .take()
            .map(|mut world| {
                world.retire();
                world
            })
            .or_else(|| self.spare_world.take());
        Self {
            spare_world,
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
        // The kitty command's one re-stamp: a latched trick waits out the
        // user's own typing, and this note is exactly one per admitted press.
        // The move sensor must not re-stamp (program output moves the caret
        // too, and a build log would hold the latch for its whole cap).
        // Navigation, paste and kill chords re-stamp nothing: the listener has
        // already poisoned or reset the line.
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

    /// Cells the last world observation classified: the pet's per-frame walk
    /// size (min(rows,64) × min(cols,256), on every presented frame), exposed
    /// so a running product can report the cost.
    ///
    /// Zero before any coherent observation and after a refused one
    /// ([`PetWorld::retire`] clears the count).
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
        world.observe_with_exclusions_near(input, facts, pane, exclusions, held_center);
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
            let new_ink = self
                .console
                .last_body
                .and_then(|r| local_ink(world, r, typed));
            self.console.contact_changed |= old_ink
                .zip(new_ink)
                .is_some_and(|(a, b)| a.iter().zip(b).any(|(old, new)| new & !old != 0));
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
        }
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
                return (first..end).try_fold(false, |occupied, c| {
                    world
                        .locomotion_ink_at(row.round() as usize, c)
                        .map(|ink| occupied || ink)
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

    /// [`Self::console_still_body_clear`]'s verdict for a still body standing
    /// at `(col, row)` of the same map in `stance` — a ghost still on glass
    /// there at the boundary (`Departure::own_clear`), at the stance it is
    /// drawn at (`Departure::stance0`) — three-valued over what the map
    /// sees of it (`PetWorld::seen_under_text_clearance_past_caret`): `None`
    /// where the part of it this map covers is clear and the rest is outside
    /// the coverage window (unseen, which is not evidence of anything), and
    /// `Some(false)` over a protected cell the map sees, or with no certified
    /// map at all, which admits nothing.
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

    /// An examined obstruction under the pet's natural body. `clear` would
    /// also answer `false` for "outside the coverage window", and this
    /// predicate authorizes a teleport, so it acts only on a certified
    /// `Some(false)`.
    ///
    /// The caret is not an obstruction to its own escort: [`STATION_LEAD`]
    /// seats the escort one cell past the caret, inside its 3x3 keep-off ring,
    /// so reading the ring as protected would evict a cat sitting exactly
    /// where the escort rule puts it. Glyphs are `ink_overlaps`'s to judge;
    /// selections, images and uncertified geometry are this one's.
    pub(super) fn console_obstructed(&self, sense: PetSense) -> bool {
        self.console.presentable
            && self.console.world.as_ref().is_some_and(|world| {
                world.under_text_clearance_past_caret(self.console_body(sense), CLEARANCE)
                    == Some(false)
            })
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
                // A held verdict is a fact about a place, and this body just
                // moved to a safe one, so retire the accumulated evidence. This
                // also keeps recovery to one fade instead of this envelope's
                // dwell and ramp on top of the 0.30 s arrival.
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
        // A body still gliding to its station has not settled yet, so the
        // dwell measures SETTLED time and a long trip cannot eat it.
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
            self.console.trip = None;
            self.console.progress_key = None;
            self.console.progress_at = None;
            self.console.result_at = None;
            self.console.contact_at = None;
            self.console.contact_armed = true;
        }
        if !self.console.presentable {
            self.console.cursor_home = None;
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
        // Only a PAINTED caret is a home: an unpainted one is a program's
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
            self.pending_sulk = false;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
            self.quiet = 0.0;
            // A commanded seat stands down on its quiet clock, and a "heard
            // you" beat the hand typed over owes its verb no longer: a key that
            // does not move the caret is still the hand going back to work.
            // The trick latch is kept: the word's own Enter is a fresh input,
            // and the verb is owed to the prompt that follows.
            self.clear_trick_settle();
            self.drop_trick_go();
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

        // A small repair holds its local subject. Long deletion releases it
        // to the existing retreat gait; arrow movement cannot create a repair.
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
            let safe = world.clear_past_caret(body, CLEARANCE);
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
                    if safe && self.facing_left {
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

        // Eligible direct affection outranks program watching; the petting
        // owner consumes the contact and its finite hold unchanged.
        //
        // A typed kitty command is the same kind of address while it has the
        // cat's attention or body ([`PetBrain::trick_engaged`]): the latch, its
        // opening beat, a one-shot still playing, a commanded seat still held.
        // Each term is finite or interruptible, and none is in
        // `console_frames`, so a resident that owns the tick still parks.
        if self.pending_pet > 0 || self.pet_hold_t > 0.0 || self.trick_engaged() {
            self.console.resident = false;
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
        // The located response replaces the old unbounded latch, never adds
        // a later cheer after the user has already resumed another activity.
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
            self.console.gaze = Some((subject.row, subject.col));
            self.console.pose = Some(if progress {
                PetGlyphId::PetPerkTurn
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
            self.console.pose = Some(if failed {
                PetGlyphId::PetInspectDown
            } else {
                PetGlyphId::PetStretchHind
            });
            self.console.target = Self::console_may_stand(world, body, CLEARANCE).then_some(body);
            self.console.resident = true;
            self.console.still = true;
        } else if let Some(anchor) = self.console.anchor.and_then(|a| {
            world.resolve(a).or_else(|| {
                // A coalesced output scroll can remove the old row while
                // later rows of the SAME observed block remain on screen.
                world
                    .anchors()
                    .iter()
                    .flatten()
                    .find(|current| current.surface == a.surface && current.block_id == a.block_id)
                    .copied()
            })
        }) {
            // One existing content anchor may survive a completed command; no
            // timer grants another excursion around the same screen.
            //
            // The perch is a visit, not a tenancy: with no live event behind
            // it, held forever it would pin pose, scale, lift and motes with no
            // frame train and no wake. [`PERCH_DWELL`] bounds the visit and
            // `console_deadline` offers its end.
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
                self.console.pose = Some(PetGlyphId::PetEdgePerch);
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
                    // The content may carry the body only while it stays on
                    // glass. `dy` is a whole inter-tick scroll, and without a
                    // floor the row would go negative and be clamped to the top
                    // row by `emit`. So a scroll that would carry the body off
                    // the top does not move it: the pet keeps its visible seat,
                    // gives up the perch (the anchor left the view), and the
                    // caret's escort re-stations it.
                    let scrolled_off_top = self.row - dy < 0.0;
                    if !scrolled_off_top {
                        self.row -= dy;
                        if let Some(trip) = self.console.trip.as_mut() {
                            trip.from.1 -= dy;
                            trip.to.1 -= dy;
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
        // Output directs attention; the caret remains home, and home is the
        // escort's stand ([`Self::console_seat`]). A certified resident is
        // kept even on ordinary ink, avoiding a fresh search per identical
        // frame.
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

    /// THE REPAINT HOLD. A DECTCEM hide means "I am repainting", so for up to
    /// [`HIDDEN_HOLD_MAX`] an unpainted caret is read as the last caret this
    /// surface PAINTED ([`ConsoleLife::cursor_home`], while the observation
    /// still vouches for it), as painted. The brain then sees a caret that has
    /// not moved, and every arm runs as on a painted frame: no chase, no move
    /// event, a flight lands where it was going, a sleeper sleeps. Past the
    /// bound the unpainted caret passes through and is followed, starting as
    /// a re-sighting: the pet did not see where it went, so the chase takes
    /// it quietly from the pet's own feet, as it would a caret that
    /// reappeared.
    ///
    /// The hold used to pin the resident's body instead, and a same-tick
    /// escort resign handed the hidden caret straight to the ordinary ladder:
    /// the pet chased the program's write position, stored it as its last
    /// caret so the caret's return read as a line change, loafed a sleeper
    /// awake, and was re-seated from its drawn rect with a flight's lift and
    /// the row-0 clamp baked into its feet.
    pub(super) fn hold_unpainted_caret(&mut self, sense: PetSense, dt: f32) -> PetSense {
        if sense.caret_drawn && sense.caret.is_some() {
            self.console.hidden_hold = 0.0;
            self.console.caret_held = false;
            return sense;
        }
        self.console.hidden_hold += dt.max(0.0);
        let vouched = || {
            let now = self.console.world.as_ref().and_then(|w| w.stamp());
            now.zip(self.console.tick_stamp)
                .is_some_and(|(now, prior)| now.preserves_cursor_home(prior))
        };
        match self.console.cursor_home {
            Some(home)
                if sense.caret.is_some()
                    && self.console.presentable
                    && self.console.hidden_hold <= HIDDEN_HOLD_MAX
                    && vouched() =>
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
        // A CARET THE PET CANNOT TAKE AT ITS WORD. The host says separately
        // whether a caret EXISTS and whether it is PAINTED ([`PetSense::caret`]
        // / [`PetSense::caret_drawn`]) since 2026-09-19, when a pet fed `None`
        // for a hidden cursor sat 20.04 s at row 0 while a DECTCEM-hiding TUI
        // worked on row 44, then crossed 28.4 cells in one frame (the owner's
        // *"trapped at the top of the screen"*). Two cases reach here:
        //
        //  * caret ABSENT — the viewport is scrolled into history. There is
        //    no better place for the cat than where it stands, so the
        //    resident holds it there, unbounded, without inventing a caret for
        //    the move sensor. It is re-seated at the brain's own seat from the
        //    last drawn frame: `choose_console_perch`'s scroll compensation may
        //    have moved it this tick, and output may not carry the resident
        //    away from input (`hidden_cursor_home_stays_put_while_real_output_scrolls`).
        //    A flight in the air is left to the no-caret arm, which eases its
        //    lift down as it fades: re-seating it from the drawn rect used to
        //    freeze the cat mid-air, its lift baked into its feet.
        //  * caret PRESENT but unpainted past [`HIDDEN_HOLD_MAX`] (inside the
        //    bound [`Self::hold_unpainted_caret`] already read it as the
        //    painted home) — a build or a pager, not a repaint.
        //
        // Admission happens AFTER begin_console_tick has handled real
        // selection, input, and OSC command facts: a hold does not erase
        // attention ownership or lose a completion.
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
            // THE HOLD OUTLASTED THE REPAINT IT WAS FOR, and the caret is in
            // plain sight of the brain even though the emulator is not drawing
            // it. Residency would survive the bound on its own, and a resident
            // sits at `console.target`, so hand the body back to the caret's
            // own escort, which follows it (measured before this: the cat kept
            // a stale seat for the whole of a 20 s build). A frame that paints
            // the caret again re-establishes residency through the usual door.
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
        self.console.resident_handoff = false;
        // A resident branch has already yielded affection to protected
        // reading or another interest; retire its suppressed latch even with
        // no safe footprint, so it cannot keep a hidden pet polling or replay
        // when the selection is released.
        self.pet_hold_t = 0.0;
        self.pending_pet = 0;
        self.pet_at = None;
        // The kitty command goes the same way, so a held selection or hidden
        // caret cannot replay a stale verb later. Exception, for the latch
        // only: while a repair is live. Fixing a typo is still typing the word
        // (`sot`, two backspaces, `it`, Space fires with the repair's hold
        // standing), and the hold ([`EDIT_HOLD`] from the last key) is well
        // inside the latch's TTL. Keyed on the repair's own clock, since
        // advancing ink labels a repair as `Contact` at first.
        let repairing = self.console.repair_until.is_some_and(|end| sense.now < end);
        let latched = self.pending_trick.filter(|_| repairing);
        self.drop_tricks();
        self.pending_trick = latched;
        let body = self.console_body(sense);
        let world = self.console.world.as_ref()?;
        let home = self.console_caret_home(sense, width);
        // The escort delivers, the resident holds. Beside a live caret the seat
        // is decided from the escort's stand every frame, never carried over
        // from a target chosen while the body was elsewhere. A body not yet at
        // the stand is the cursor follower's to move (its pace, row-hop path,
        // lead decay and wall hysteresis) to the rect this arm calls home. A
        // body off glass or under reduced motion is placed directly.
        //
        // A repair or a result holds its local subject: those arms claim the
        // body where it is (an inspection, an exit flourish), admitted only on
        // a body that may stand, within [`HOME_REACH`] of the stand, else the
        // caret has first claim.
        //
        // With the caret hidden (or no home) there is no stand, so the
        // admitted body is held where it stands.
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
        self.clear_play();
        self.pending_pounce = false;
        self.pending_big_jump = false;
        self.pending_bell = None;
        self.pending_flow = None;
        self.pending_perk = None;
        self.pending_catch = None;
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
                let x = trip.from.0 + (trip.to.0 - trip.from.0) * u;
                let y = trip.from.1 + (trip.to.1 - trip.from.1) * u;
                let travelled = (x - self.col).abs();
                self.speed = (trip.to.0 - trip.from.0) / trip.duration;
                self.col = x;
                self.row = y;
                self.stride = (self.stride + travelled / STRIDE_CELLS).rem_euclid(1024.0);
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
                        PetGlyphId::PetHop
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
            if let Some((r, c)) = self.console.gaze {
                self.facing_left = c < self.col + width * 0.5;
                // First placement and the last transit frame use the gaze
                // from the final station. No idle redraw is needed to settle.
                if matches!(
                    self.console.attention,
                    PetAttention::Reading | PetAttention::Output
                ) {
                    self.console.pose = Some(if r < self.row - 0.5 {
                        PetGlyphId::PetEdgeLookUp
                    } else if r > self.row + 0.5 {
                        PetGlyphId::PetEdgeLookDown
                    } else if self.console.attention == PetAttention::Reading {
                        PetGlyphId::PetEdgePerch
                    } else {
                        PetGlyphId::PetEdgeLean
                    });
                }
            }
        } else {
            self.facing_left = self.speed < 0.0;
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
    /// The correction searches with [`PetWorld::nearest_under_text_perch`], not
    /// `home_perch`: the escort takes the closest place it may stand.
    pub(super) fn console_station(
        &self,
        desired: (f32, f32),
        sense_dims: (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        if !self.console.presentable {
            return None;
        }
        let world = self.console.world.as_ref()?;
        world.stamp()?;
        let body = Self::console_stand_rect(desired, sense_dims.0, width);
        if world.under_text_clearance_past_caret(body, CLEARANCE) != Some(false) {
            return None;
        }
        // No perch in reach is `None`, not "stay where you are": both callers
        // fall back to the baseline station, so the pet keeps following the
        // caret.
        let next = world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH)?;
        Some((next.col, next.foot_row()))
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

    /// The resident's home is the escort's stand: one distance from the caret.
    /// (`tests/pet_resident_standoff.rs` pins that they agree.)
    ///
    /// Uses the escort's rule as `station_safe` gives it to the chase: the
    /// lead-free [`PetBrain::station`] (a parked hand has no rhythm to lead)
    /// with a controller lease's far stand folded in, run through the ink
    /// ladder, and corrected by [`Self::console_station`] where the escort's
    /// stand would be.
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
    ///  * [`CaretSeat::Hold`]: a body the escort has delivered stays. It is at
    ///    rest (`speed` under the `needs_frames` floor), within the chase's
    ///    [`ARRIVED`] tolerance of the stand on its row, and may stand there.
    ///    Correcting the chase's small arrival residue would slide a settled
    ///    cat every time a command started.
    ///  * [`CaretSeat::Escort`]: anywhere else is the escort's to close. Only
    ///    a positive observation of a protected surface refuses the stand
    ///    ([`Self::console_must_yield`]); the keep-off ring and ordinary ink do
    ///    not. This arm does not walk the body itself: the chase already does,
    ///    to the same rect, and a second mover is what
    ///    `pet_position_authority` refuses.
    ///  * [`CaretSeat::Yield`]: a protected stand. `console_caret_home` already
    ///    applied the escort's correction ([`Self::console_station`]), so the
    ///    caller asks [`PetWorld::nearest_under_text_perch`] once more and
    ///    yields on `None`.
    fn caret_seat(&self, world: &PetWorld, body: PetRect, stand: PetRect) -> CaretSeat {
        if self.speed.abs() <= 0.01
            && Self::at_console_stand(body, stand)
            && Self::console_may_stand(world, body, CLEARANCE)
        {
            CaretSeat::Hold
        } else if !Self::console_must_yield(world, stand) {
            CaretSeat::Escort
        } else {
            CaretSeat::Yield
        }
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
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        let Some(rect) = body_rect(frame, sense) else {
            return;
        };
        // The console layer is not a second opinion about position. Distance
        // from the caret is the escort's business (its chase at the shipped
        // speed, and the resident handoff in [`Self::tick_console_resident`]).
        // Pulling a far body home here would make two writers with different
        // paces fight over it. What remains is the one correction the escort
        // cannot make: a body on a selection, an image, or a row with no
        // certified cell-to-pixel projection.
        if !Self::console_must_yield(world, rect) {
            return;
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
        let (dx, dy) = (shifted.col - frame.col, shifted.row - frame.row);
        *frame = shifted;
        self.console.reason = "cursor-home";
        self.col += dx;
        self.row += dy;
        self.col_at_tick += dx;
        // A STILL GHOST IS THIS BODY A FRAME AGO — the fading half of an
        // in-place crossfade — so it steps aside with the body, or the swap
        // frame splits one cat into two a perch apart. A running ghost is
        // its own motion and keeps its door, and a re-sighting ghost is not
        // this body at all: it is the replaced surface's body, standing where
        // the user saw it (on its own glass, in another pane's cells).
        for (record, drawn) in self.departures.iter_mut().zip(frame.departures.iter_mut()) {
            if let Some(record) = record.as_mut().filter(|d| d.still && !d.resight) {
                record.from_col += dx;
                record.edge += dx;
                record.row += dy;
                if let Some(drawn) = drawn.as_mut() {
                    drawn.col += dx;
                    drawn.row += dy;
                }
            }
        }
        if let Some(flight) = self.flight.as_mut() {
            flight.from_col += dx;
            flight.to_col += dx;
            flight.from_row += dy;
            flight.to_row += dy;
        }
        if let Some((col, row, _)) = self.bound2.as_mut() {
            *col += dx;
            *row += dy;
        }
        if let Some((col, row)) = self.deferred_hidden_landing.as_mut() {
            *col += dx;
            *row += dy;
        }
        if let Some(target) = self.console.target.as_mut() {
            target.col += dx;
            target.row += dy;
        }
        if let Some(trip) = self.console.trip.as_mut() {
            trip.from.0 += dx;
            trip.to.0 += dx;
            trip.from.1 += dy;
            trip.to.1 += dy;
        }
    }

    pub(super) fn finish_console_frame(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if !self.console.presentable {
            return;
        }
        if let Some(pose) = self.console.pose {
            // The contact is a pose over the existing displacement; flights
            // keep their own silhouette unless this is the console's trip. A
            // settled cat's own animation (sit, loaf, groom, sleep) is left
            // alone while typing: the layer has nothing to correct there.
            // Contact (advancing ink running into the body, a 0.40 s hold)
            // still applies.
            if !self.console_legacy_motion_pending()
                && self.action != PetAction::Land
                && (self.console.resident || self.console.attention == PetAttention::Contact)
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
            // NO CERTIFIED MAP AT ALL — not "I cannot see this rectangle" but
            // "I cannot see anything": a stale snapshot, a resize frame whose
            // cells and facts straddle two grids. Nothing new is licensed
            // here — the tick froze the body where it stood, and no station,
            // perch or ghost is judged against a map that does not exist.
            frame.motes = [None; PET_MOTES_MAX];
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "incoherent-surface";
            if self.console.on_glass && frame.alpha > 0 {
                // A BODY ALREADY ON GLASS IS HELD, not cut (2026-10-01). It
                // stands exactly where the last certified frame judged it,
                // and "I cannot see" is the visibility hold's own unknown: it
                // keeps the verdict that body already had, and only an
                // unbroken blind run of [`VETO_BLIND`] counts as obstruction
                // and fades it over [`VETO_FADE`]. Cutting to alpha zero here
                // — the shipped rule until then — blinked the resident for
                // one frame on every incoherent read: a resize frame, a
                // snapshot a present reused. The z-order is held for the
                // same reason: a body under the glyphs stays under them.
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
            // NOTHING WAS ON GLASS: the pet is drawn only over pixels an
            // observation certified, so a first appearance on an incoherent
            // surface draws nothing.
            //
            // This bypasses the visibility hold: momentary incoherence is not
            // evidence of ink, must not spend the dwell, and must not clear
            // `on_glass` (which would grant the next reseat a teleport).
            frame.alpha = 0;
            frame.lane_alpha = 0;
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.clipped = true;
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
            // The visibility verdict is three-valued: `None` ("cannot see
            // there", e.g. outside the caret-centred coverage window) is not
            // evidence of ink, and the hold keeps the previous verdict on it.
            //
            // `under_text_clearance_past_caret`:
            //
            //  * under text: ordinary ink in front of the body is a z-order
            //    fact (`frame.under_ink` below), not a reason to hide;
            //  * past caret: placement keeps the caret protected so the pet
            //    never rests on it, but a pet walking past it must not vanish
            //    for the frames it overlaps. The escort lives in that ring.
            //
            // A frame with no body goes to `idle`, not `update`.
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
            // ORDINARY TERMINAL INK IS IN FRONT OF THE FULL BODY. A busy
            // prompt must not switch a resident off or shrink it — it
            // changes the z-order and nothing else, and that z-order is
            // HELD (`ZHold`). Only real INK is asked about, never the
            // caret's keep-off ring: an unobserved rectangle still answers
            // inked and draws under the text, the safe direction for a
            // layering decision.
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
        for mote in &mut frame.motes {
            if mote.is_some_and(|m| {
                // Conservative around the center, including minimum tile
                // size, scaling and pixel rounding on every side.
                let radius = f32::from(sense.cell_h.max(6)) * m.scale + 1.0;
                let rw = radius / f32::from(sense.cell_w.max(1));
                let rh = radius / f32::from(sense.cell_h.max(1));
                !world.clear(
                    PetRect::new(m.row - rh, m.col - rw, 2.0 * rh, 2.0 * rw),
                    CLEARANCE,
                )
            }) {
                *mote = None;
            }
        }
        // One crossfade, one visibility rule. The emitter draws a departing
        // ghost at `d.alpha × lane_alpha / 255`, and `lane_alpha` carries this
        // frame's cover, so body and ghost ramp together (a body at alpha 0
        // took every ghost with it above). The ghost's own rectangle gets the
        // body's rule:
        //
        //  * selected text subordinates it at once;
        //  * nothing else culls a ghost by itself (an obstruction gets the
        //    body's dwell and ramp).
        //
        // A ghost with no on-grid rectangle at all is dropped: there is
        // nothing to draw.
        //
        // THE RE-SIGHTING GHOST is the one exception, because it is the one
        // ghost whose rectangle the body never stood on in THIS world: it is
        // the old surface's body fading on glass a new screen now owns
        // (`Departure::resight`). It is kept only where this map certifies
        // nothing protected — no selection, image or overlay — and it goes
        // under whatever ink the new screen has there, so the text in front
        // of it stays legible. Where this map cannot see it (outside its
        // caret-centred coverage window: a tall screen whose caret is far
        // from where the old body stood), it keeps the verdict its own
        // surface gave it at the boundary (`Departure::own_clear`), beneath
        // whatever ink is there — unknown is not evidence, the visibility
        // hold's rule for a body already on glass — while any protected cell
        // the map does see under it still refuses it
        // (`PetWorld::seen_under_text_clearance_past_caret`). Culling it
        // there left the arriver alone at its ramp's floor (measured: 15/255
        // on the first frame of a 90-row alternate screen). So its guarantee
        // is NEVER OVER A PROTECTED CELL THIS MAP SEES: a protected cell of
        // the new screen outside the window is unknown, and the ghost may
        // fade over it for its quarter second, beneath the ink (an inline
        // image is drawn above the under-text layer in both renderers, and
        // a double-width row's glyphs over the ghost). Over anything
        // protected it sees it is dropped, and its presence is not lost: it
        // goes to its arriver from this frame on (below), so the cat stands
        // at its new station at the presence the user saw instead of
        // vanishing to the floor.
        //
        // A GHOST ON ITS OWN GLASS (`Departure::own_glass`) is not on this
        // map at all: it stands in the pane the focus left, in that pane's
        // cells, and this world is the new pane's. Its own surface admitted it
        // at the boundary (`PetBrain::displace_resight`), and it stays
        // beneath that pane's ink, which nobody observes after it.
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
        // A REFUSED CROSSFADE GHOST HANDS ITS PRESENCE TO ITS ARRIVER. The
        // ghost and the arriver are one cat changing places on one ramp; a
        // ghost that may not be drawn — something protected under it in this
        // world, or no rectangle on this grid — puts what it would have shown
        // on the arriver instead, never more than the lane's presence. The
        // cat moves to its new station at once rather than vanishing to the
        // ramp's floor and fading back in.
        if refused > 0 {
            frame.alpha = (u16::from(frame.alpha) + refused).min(u16::from(frame.lane_alpha)) as u8;
        }
        // THE HAND-OVER IS FOR GOOD. The refused ghost left the lane above,
        // and the arriver's ramp starts from its share: lifting `arrive_from`
        // by `envelope / remaining` adds exactly the ghost's envelope to every
        // later frame's arrival, `envelope × remaining_then / remaining_now`
        // (equal to the ghost's own fade on the ramp they share, its peak
        // times what is left). A refusal judged afresh on every frame,
        // with the ghost kept in the lane, counted it twice: carried to the
        // next boundary beside the boosted body as presence the user saw
        // (measured: a protected alternate screen quit two frames after it
        // was entered summed 492/255 on the first frame back, a cat popping
        // in beside the ghost), and re-admitted when the protection lifted
        // mid-fade (the arriver fell 206/255 in one frame while the ghost
        // reappeared at the old seat). No ramp left, nothing is owed: the
        // arriver is already at the lane's presence.
        let remaining = self.arrive_t / self.arrive_in;
        if handed > 0.0 && remaining > 0.0 {
            self.arrive_from = (self.arrive_from + handed / remaining).min(1.0);
        }
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
                || (!self.console.clipped && (self.console.moving || self.alpha < 1.0)),
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
        .filter(|at| *at > self.last_now.unwrap_or(now))
        .map(|at| at.max(now + ARM_MIN))
        .min()
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

/// A body standing still at `(col, row)` in `stance` — [`Stance::NEUTRAL`]
/// for the natural body, no lift, no squash — in fractional grid cells: the
/// `PetFrame::body_px` projection itself (`drawn_body_px`), including the
/// origin, the feet and the grid clamp. Fractional flight landings can
/// straddle a protected cell before rounding even though the actual
/// displayed body clears it.
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

/// The caret's OWN writing this frame, pane-local `(row, first, end)`: the
/// cells between the last ticked caret and the visible one on its row (the
/// cell just behind the caret when the caret changed rows or went unseen),
/// at most [`TYPED_RUN_MAX`]. A glyph typed there is the escort's next step,
/// not ink arriving at the cat: counted as contact it braced the escort on
/// the first key of every burst, the brace sliding sideways under the chase.
/// Ink anywhere else — a completion, output, text the caret did not write —
/// still touches.
fn typed_run(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
    last: Option<(u16, u16)>,
) -> Option<(usize, usize, usize)> {
    if !input.cursor_visible || input.display_offset != 0 {
        return None;
    }
    let row = input.cursor_row.checked_sub(pane.row)?;
    let col = input.cursor_col.checked_sub(pane.col)?;
    let first = last
        .filter(|&(r, c)| usize::from(r) == row && usize::from(c) < col)
        .map_or(col.saturating_sub(1), |(_, c)| usize::from(c));
    Some((row, first.max(col.saturating_sub(TYPED_RUN_MAX)), col))
}

/// Compare occupied ink at the SAME body rectangle before/after observation.
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
        for c in c0..c1 {
            match world.ink_at(r, c) {
                None => return None,
                Some(true)
                    if !typed
                        .is_some_and(|(row, first, end)| r == row && (first..end).contains(&c)) =>
                {
                    let i = (r - r0) * 32 + c - c0;
                    bits[i / 64] |= 1 << (i % 64);
                }
                Some(_) => {}
            }
        }
    }
    Some(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // Record this exact footprint with blank space at its near edge.
        s.pet.col = 20.0;
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

    /// THE INCOHERENT FRAME HOLDS, AND ONLY HOLDS (2026-10-01). A stale
    /// snapshot — the cells read before the terminal changed — certifies
    /// nothing, so it licenses nothing: no move toward the caret it reports,
    /// no legacy placement, no first appearance. But a body ALREADY on glass
    /// is the visibility hold's unknown, not its obstruction: it stays where
    /// the last certified frame judged it, at its opacity, until an unbroken
    /// blind run of `VETO_BLIND` counts as obstruction and fades it over
    /// `VETO_FADE`. The shipped rule cut it to alpha zero on the first
    /// incoherent frame — the one-frame blink of a resize frame or a reused
    /// snapshot. CONTROLS: the hold ends (an unbroken blind run fades the
    /// body out), and a pet that was not on glass still draws nothing.
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

        // A pet that was not on glass is never MATERIALIZED by an
        // uncertified map — the original law, unchanged.
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
        // That future is its own life: the perch is spent, so the pet goes back
        // to escorting the caret and the idle ladder. The budget is derived
        // from [`PERCH_DWELL`] so the test runs past the bound on its own.
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

    /// The escort: when no perch is in reach, `console_station` answers `None`
    /// rather than "stay where you are", so the pet keeps following the caret.
    ///
    /// The fixture uses double-width lines: a resident may stand under
    /// ordinary ink, but a DECDWL line has no certified cell-to-pixel
    /// projection, so every cell is `Protected` and the search comes back
    /// empty.
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

    /// Tier 1 checks the genuine retained brain field, not just a body that
    /// might happen to remain nearby after losing its owner. Every snapshot
    /// comes from a real Terminal; no input intent is manufactured.
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

    /// THE GHOST ON ITS OWN GLASS IS ADMITTED BY ITS OWN WORLD, AT THE
    /// BOUNDARY (`PetBrain::displace_resight`): a split-pane focus switch
    /// spends the old body in the pane the focus left only where that pane's
    /// last observation — the one the body was drawn against — certified
    /// nothing protected under it. Spent, it stands at the old body's cell
    /// beneath that pane's text while the arriver comes up at its own
    /// station. CONTROL: a selection under the body in that observation
    /// refuses it (what the arriver does with its presence then is
    /// `pet_coordinate_seams`' `a_ghost_its_own_glass_refuses_…`).
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

    /// A GHOST IS CARRIED ACROSS A BOUNDARY AT WHAT ITS OWN SLOT DREW
    /// (`PetBrain::last_drawn`), slot for slot: one in the lane's second slot
    /// behind an empty first is carried at that slot's presence — a cover
    /// that dimmed it to 0.4, not its envelope's full peak. CONTROL: a slot
    /// that drew nothing is not carried.
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

    /// MEASURED ON GLASS (0.83.0 with the crossfade fix, the Claude-Code-
    /// shaped streamer, a look swap landing between two bracketed repaints
    /// while `protected-displacement` held the body — the skeptic's
    /// after-cascade_typing take, frames 398-412): the presented frame
    /// carried the ARRIVER at 36 % and NO ghost, then the 0.25 s ramp. The
    /// freeze was gone; the blink was not. The resident arm emptied the
    /// departure lane on its first tick and `finish_console_frame` emptied
    /// the frame's again, while the arriving body on the very same tick was
    /// drawn through the hold like any other — two halves of one crossfade
    /// under two laws.
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
        // The debounced handoff lands on a FREE tick — the streamer's
        // caret-visible frame between two repaints — never on a resident's.
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
    //
    // The brain's own trick tests run with no console world, so
    // `begin_console_tick` returns before it can wipe a latch or seat a
    // resident. These drive the real terminal, input notes and OSC 133 block
    // states.

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

    /// …and so does a ONE-SHOT: a roll asked for during a build plays out
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
}
