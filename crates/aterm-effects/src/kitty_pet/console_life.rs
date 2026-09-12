// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Located console attention within the resident brain. Input is intent;
//! only coherent displayed occupancy licenses contact and placement.

use super::*;
use crate::pet_world::TaskbarProgress;
use crate::pet_world::{PetAnchor, PetPane, PetRect, PetWorld, PetWorldFacts, PetWorldStamp};
use aterm_core::terminal::BlockState;

const INPUT_HOLD: f32 = 0.65;
const EDIT_HOLD: f32 = 1.10;
const CONTACT_HOLD: f32 = 0.40;
const RESULT_HOLD: f32 = 1.20;
const REACTION_TTL: f32 = 2.0;
const CLEARANCE: f32 = 0.10;
const CONTACT_MARGIN: f32 = 0.45;
const PERCH_REACH: f32 = 24.0;
/// The caret is home. Located output may direct the gaze, but must not park
/// the resident across the pane. Rows cost twice a column in this bound.
const HOME_REACH: f32 = 6.0;
// Leave two cells for a newly typed character and the follower's acceleration.
// A station exactly touching the cursor's protected halo would blink out on
// the next key before a physical body could move away.
const HOME_BREATHING_ROOM: f32 = 2.0;

/// How long a SPENT interest — a content perch with no live event left to
/// react to — may hold the resident before the pet is handed back its own
/// life. The perch is a visit, not a tenancy: without this the console layer
/// answers `console_frames() == Some(false)` and `console_deadline() == None`
/// forever, which is a pet with no future at all — it can never breathe,
/// blink, settle, sleep, or notice that the caret has walked away.
const PERCH_DWELL: f32 = 3.0;

/// THE VISIBILITY HOLD's dwell, in BOTH directions.
///
/// v0.81.0 made the pet's presence a per-frame boolean function of screen
/// content. Measured on glass over one 1.67 s typing burst, the cat's alpha
/// ran 1.00, 0.00, 0.17, 0.29, 0.57, 0.43, 0.37, 0.29, 0.67, 0.45, 0.67,
/// 0.51, 0.00 — six direction reversals in 950 ms, a half-period of ~158 ms.
/// A hold shorter than that half-period does not remove the strobe, it only
/// slows it; this is 1.6x longer. It is also the budget the pet has to WALK
/// out of the way before it has to fade instead, which is why it is not
/// longer still.
const VETO_DWELL: f32 = 0.25;
/// A HARD CUT is what the owner saw. When an obstruction is real and
/// sustained the pet yields over a RAMP instead — a fifth of a second, which
/// reads as the cat stepping aside and is short enough that it is not
/// painting over the user's text for half a second first. The ramp back IN
/// is normally the pet's own 0.30 s arrival fade, because a body hidden long
/// enough to be reseated returns at a NEW station (see
/// `reseat_unshown_console_body`), and a station change retires the verdict
/// this envelope was holding.
const VETO_FADE: f32 = 0.20;
/// Blindness is not a licence to stay forever. "I cannot see there" keeps the
/// previous verdict — that is the entire point of the three-valued answer —
/// but an unbroken run of unknowns this long (a surface that never regains
/// coherence) finally counts as obstruction, so a retired world can never
/// strand an opaque pet on glass.
///
/// This counts only unknowns about a body THAT IS BEING EMITTED. A frame with
/// no body at all goes to [`VisibilityHold::idle`] and never reaches here.
const VETO_BLIND: f32 = 0.50;

/// The pet's presence as a HELD STATE rather than a per-frame boolean.
///
/// Fed the three-valued clearance verdict for the emitted sprite rectangle,
/// it answers with a 0..=1 cover factor. Two properties do the work:
///
///  * an `Unknown` verdict neither flips the state nor resets a run in
///    progress — it is not evidence, so it cannot blank the pet;
///  * contrary evidence must persist for [`VETO_DWELL`] before the held
///    verdict changes, and the cover then RAMPS over [`VETO_FADE`].
///
/// A single obstructed frame therefore cannot blank the pet, and a single
/// clear frame cannot snap it back on.
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
    /// Was the hidden state entered BY FIAT — a selection, or a placement
    /// search that came back empty — rather than by perception?
    ///
    /// Those two paths yield AT ONCE by design, and the dwell exists to
    /// discount perception WOBBLE, which neither of them is. So neither may
    /// they be charged the dwell on the way back: a selection that ends is
    /// the user's act ending, and a placement search that succeeds has
    /// SEARCHED, not guessed. Without this the pet stayed dark for 0.25 s
    /// after a selection was cleared, which the cursor-home suite catches
    /// (`full_selection_still_protects_the_console_and_explains_the_hidden_body`).
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
        // A YIELD BY FIAT IS NOT PAID FOR TWICE. `hide_now`'s two callers cut
        // at once on purpose; the first verdict that says the ground is free
        // again therefore restores at once too, rather than spending a dwell
        // meant for perception wobble on the end of a user's selection.
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

    /// NOTHING WAS DRAWN AT ALL, which is not the same `None` as "I cannot
    /// see there" and must not be spent as one.
    ///
    /// The emitter produces no body of its own accord all the time: an
    /// arrival fade that has not started, a resident that has finished its
    /// own 0.30 s retirement, a capture with no caret. There is no rectangle
    /// for the map to be blind ABOUT, so this advances the ramp and touches
    /// no run — in particular it may not feed [`VETO_BLIND`], which exists
    /// for the opposite case (a body IS being emitted and the map cannot
    /// certify the ground under it). Conflating the two latched `shown =
    /// false` on a pet that had merely faded out, and then charged it a
    /// [`VETO_DWELL`] it never owed when it came back.
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

/// A producer that KNOWS its edit range can authorize a paw. Arbitrary PTY
/// redraws are never promoted to this witness by timing or text matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetEditPhase {
    Vacated,
    Replaced,
}

#[derive(Clone, Copy, Debug)]
struct InputEpisode {
    seq: u64,
    kind: PetInputKind,
    at: Instant,
    target: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
struct EditWitness {
    seq: u64,
    stamp: PetWorldStamp,
    range: PetRect,
    phase: PetEditPhase,
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
    presentable: bool,
    seq: u64,
    input: Option<InputEpisode>,
    consumed: u64,
    edit: Option<EditWitness>,
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
    /// THE ONE EXPLICIT FACT the reseat needs: did the last emission actually
    /// put pixels on glass? This used to be inferred from `last_body` being
    /// `None`, which the visibility veto ALSO wrote — so a vetoed frame read
    /// as "never drawn", teleported the pet and restarted its fade from zero.
    on_glass: bool,
    hold: VisibilityHold,
    /// Last real visible caret on this coherent coordinate surface. This is
    /// placement memory only; it never enters the keyboard/motion sensor.
    cursor_home: Option<(u16, u16)>,
    contact_changed: bool,
    observed_seq: u64,
    progress_seq: u64,
    /// When the SETTLED spent perch was taken. `Some` only while the content
    /// perch owns the resident with nothing live to attend; the dwell it
    /// starts is offered by [`PetBrain::console_deadline`] so the wake that
    /// releases it actually happens.
    perched_since: Option<Instant>,
}

impl PetBrain {
    /// Exactly one note per admitted gesture/bundle. A later output echo
    /// may complete an explicit witness, but never creates another input.
    pub fn note_console_input(&mut self, now: Instant, kind: PetInputKind) {
        let repair_live = self.console.repair_until.is_some_and(|end| now < end);
        if kind == PetInputKind::Delete || (kind == PetInputKind::Text && repair_live) {
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
        self.console.edit = None;
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

    pub fn observe_console_with_exclusions(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
    ) {
        let world = self
            .console
            .world
            .get_or_insert_with(|| Box::new(PetWorld::default()));
        let prior_stamp = world.stamp();
        let prior_selection = world.selection_target();
        let prior_progress = world.progress();
        let old_ink = self.console.last_body.and_then(|r| local_ink(world, r));
        world.observe_with_exclusions(input, facts, pane, exclusions);
        if world.stamp().is_some()
            && (prior_stamp != world.stamp()
                || prior_selection != world.selection_target()
                || prior_progress != world.progress())
        {
            self.console.observed_seq = self.console.observed_seq.wrapping_add(1).max(1);
        }
        let new_ink = self.console.last_body.and_then(|r| local_ink(world, r));
        let fixed_view = prior_stamp.zip(world.stamp()).is_some_and(|(a, b)| {
            a.surface == b.surface
                && a.top_absolute_row == b.top_absolute_row
                && a.display_offset == b.display_offset
                && a.uniform_up_rows == b.uniform_up_rows
        });
        if fixed_view {
            // Multiple coherent reads may precede one tick. A duplicate
            // observation cannot erase new local ink already observed.
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

    /// Ownership/custody is independent of whether a live caret exists.
    /// This permits a valid reading anchor without manufacturing a caret.
    pub fn set_console_presentable(&mut self, presentable: bool) {
        if self.console.presentable != presentable {
            // Custody changed hands. A frozen ramp and a stale verdict from
            // the previous custodian are not evidence about this one.
            self.console.hold = VisibilityHold::default();
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

    /// Explicitly attributed edit geometry from an application adapter.
    /// The adapter must supply the current input sequence and surface stamp;
    /// stale reports and unobserved ranges are refused, never queued.
    pub fn note_console_edit(
        &mut self,
        input_seq: u64,
        stamp: PetWorldStamp,
        range: PetRect,
        phase: PetEditPhase,
    ) -> bool {
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        if input_seq != self.console.seq
            || self.console.input.is_none()
            || world.stamp() != Some(stamp)
            || !range.valid()
        {
            return false;
        }
        let coverage = world.coverage();
        let r1 = range.row + range.rows;
        let c1 = range.col + range.cols;
        if !r1.is_finite()
            || !c1.is_finite()
            || range.row < coverage.row as f32
            || range.col < coverage.col as f32
            || r1 > (coverage.row + coverage.rows) as f32
            || c1 > (coverage.col + coverage.cols) as f32
            || !(range.row.floor() as usize..r1.ceil() as usize).all(|r| {
                (range.col.floor() as usize..c1.ceil() as usize)
                    .all(|c| world.ink_at(r, c).is_some())
            })
        {
            return false;
        }
        if phase == PetEditPhase::Vacated && !world.clear(range, 0.0) {
            return false;
        }
        self.console.edit = Some(EditWitness {
            seq: input_seq,
            stamp,
            range,
            phase,
        });
        true
    }

    /// Natural, pixel-rounded sprite coverage in fractional grid cells.
    fn console_body(&self, sense: PetSense) -> PetRect {
        let ch = f32::from(sense.cell_h.max(1));
        let cw = f32::from(sense.cell_w.max(1));
        let h = (ART_ROWS * ch).round().clamp(1.0, f32::from(u16::MAX));
        let w = (h * ART_ASPECT).round().clamp(1.0, f32::from(u16::MAX));
        // Match the neutral-scale body_px projection, including the origin
        // and feet. Fractional flight landings can straddle a protected cell
        // before rounding even though the actual displayed body clears it.
        let x = (self.col * cw)
            .round()
            .clamp(0.0, (f32::from(sense.cols) * cw - w).max(0.0));
        let y = (((self.row + 1.0) * ch).round() - h)
            .clamp(0.0, (f32::from(sense.rows) * ch - h).max(0.0));
        PetRect::new(y / ch, x / cw, h / ch, w / cw)
    }

    /// An EXAMINED obstruction under the pet's natural body. `clear` would
    /// answer the same `false` for "outside the coverage window", and this
    /// predicate authorizes a teleport — so it takes the three-valued answer
    /// and acts only on a certified `Some(false)`.
    ///
    /// THE CARET IS NOT AN OBSTRUCTION TO ITS OWN ESCORT, which is why this
    /// reads past it. The strict reading counts the caret's 3x3 keep-off
    /// ring as protected, and [`STATION_LEAD`] seats the escort one cell
    /// past the caret — inside that ring by construction. With this
    /// predicate in the eviction disjunct (`kitty_pet.rs`, beside the
    /// v0.76 `ink_overlaps` test) the strict reading evicted a settled cat
    /// for sitting exactly where the escort law puts it, and every escort
    /// station restored above would have been given straight back. Glyphs
    /// are still `ink_overlaps`'s to judge; selections, images and
    /// uncertified geometry are still this one's.
    pub(super) fn console_obstructed(&self, sense: PetSense) -> bool {
        self.console.presentable
            && self.console.world.as_ref().is_some_and(|world| {
                world.under_text_clearance_past_caret(self.console_body(sense), CLEARANCE)
                    == Some(false)
            })
    }

    pub(super) fn reseat_unshown_console_body(&mut self, sense: PetSense, width: f32) {
        // NOTHING WAS ON GLASS last emission. A first appearance or a fully
        // faded-out resident may therefore start its finite fade at a current
        // safe station; a body the user can still SEE must use locomotion.
        // This used to key on `last_body.is_some()`, which the visibility
        // veto cleared on every vetoed frame — so a visible pet teleported.
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
                // A HELD VERDICT IS A FACT ABOUT A PLACE. This body just
                // moved to a different one, chosen because it is safe, so
                // the accumulated "obstructed" evidence belongs to the
                // station it left. Retiring it here is also what keeps the
                // recovery to ONE fade: without it the pet paid this
                // envelope's dwell and ramp a second time on top of its own
                // 0.30 s arrival, and stayed invisible for half a second
                // after it had already stepped somewhere clear.
                self.console.hold = VisibilityHold::default();
            }
        }
    }

    /// Read once per brain tick, before choosing between caret work and a
    /// located reading/output interest. No host classifies pet behavior.
    pub(super) fn begin_console_tick(&mut self, sense: PetSense, width: f32) {
        let contact_changed = core::mem::take(&mut self.console.contact_changed);
        // Taken, not read: only the spent-perch arm below re-establishes it,
        // so every other verdict — a key, a selection, output, a result, a
        // surface change, an incoherent read — restarts the dwell by simply
        // not claiming it.
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
            self.console.edit = None;
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
            return;
        }
        if let Some(caret) = sense.caret {
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
            self.console.target = if Self::near_console_home(body, home)
                && Self::console_may_stand(world, body, CLEARANCE)
            {
                Some(body)
            } else {
                world.home_perch(home.unwrap_or(body), CLEARANCE, HOME_REACH)
            };
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
            if repairing && Self::console_may_stand(world, body, CLEARANCE) && self.flight.is_none()
            {
                self.console.target = Some(body);
                self.console.resident = true;
                self.console.still = true;
            }
            if let Some(edit) = self.console.edit
                && edit.seq == self.console.seq
                && edit.stamp == stamp
                && self.console.input.is_some_and(|e| age(e.at) < EDIT_HOLD)
            {
                self.console.gaze = Some((edit.range.row, edit.range.col));
                self.console.pose = Some(match edit.phase {
                    PetEditPhase::Vacated if world.clear(edit.range, 0.0) => {
                        PetGlyphId::PetReachPaw
                    }
                    _ => PetGlyphId::PetWithdrawPaw,
                });
                self.console.reason = "attributed-edit-range";
            }
            // The clearance margin is real contact geometry. Never brace on
            // input alone or on a duplicate frame of the same displayed ink.
            // ADVANCING INK, and the caret's blank ring is not ink. Read
            // strictly, a cat seated at the caret's shoulder is never
            // `roomy`, so `contact_armed` can never re-arm and the whole
            // brace fires once per surface and then never again.
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

        // Eligible direct affection outranks program watching. The ordinary
        // petting owner consumes the contact and its finite hold unchanged.
        if self.pending_pet > 0 || self.pet_hold_t > 0.0 {
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
            // One existing content anchor may survive a completed command.
            // No timer grants another excursion around the same old screen.
            //
            // THE PERCH IS A VISIT, NOT A TENANCY. This interest has no live
            // event behind it — the command is over, the anchor merely still
            // resolves — so it is the one resident state nothing can ever
            // end. Held forever it made the pet a decal: the resident branch
            // pins pose, scale, lift and motes every frame, `console_frames`
            // answers `Some(false)` and `console_deadline` answered `None`,
            // so the brain owned NO FUTURE — no frame train and no wake
            // instant — and could not breathe, blink, settle, sleep or
            // notice the caret walking away. [`PERCH_DWELL`] bounds the
            // visit; `console_deadline` offers its end, so the wake that
            // releases it is real, and the pet goes back to its own life.
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
        let _ = width;
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
                    self.row -= dy;
                    body.row -= dy;
                    leaving_view = dy > 0.0 && target.row < 2.0;
                    if let Some(trip) = self.console.trip.as_mut() {
                        trip.from.1 -= dy;
                        trip.to.1 -= dy;
                    }
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
        // Output directs attention; the caret remains home. A certified
        // resident is retained even on ordinary ink, avoiding a fresh search
        // for every identical frame of a busy console.
        let next = if Self::near_console_home(body, home)
            && Self::console_may_stand(world, body, CLEARANCE)
        {
            Some(body)
        } else {
            world.home_perch(home.unwrap_or(body), CLEARANCE, HOME_REACH)
        };
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

    /// New interests use the resident's position, distance-driven gait and
    /// authored poses. This is one branch of PetBrain::tick, not a host driver.
    pub(super) fn tick_console_resident(
        &mut self,
        sense: PetSense,
        width: f32,
        dt: f32,
    ) -> Option<PetFrame> {
        // Coding consoles hide the logical cursor while repainting. Preserve
        // the resident's last real home without inventing a caret for the move
        // sensor. Admission happens AFTER begin_console_tick has handled real
        // selection, input, and OSC command facts: hidden pixels do not erase
        // attention ownership or lose a completion.
        if self.console.presentable
            && sense.caret.is_none()
            && self.console.cursor_home.is_some()
            && self.alpha > 0.0
        {
            if let Some(held) = self.console.last_body {
                self.col = held.col;
                self.row = held.foot_row();
            }
            self.flight = None;
            self.bound2 = None;
            self.land_t = 0.0;
            self.retired_flight_lift = None;
            self.deferred_hidden_landing = None;
            self.console.resident = true;
            self.console.target = Some(self.console_body(sense));
            self.console.trip = None;
            if self.console.attention == PetAttention::Rest {
                self.console.reason = "cursor-hidden-home";
                self.console.pose = Some(PetGlyphId::PetLoaf);
            }
            self.console.still = true;
            self.last_caret = None;
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
        // reading or another selected interest. Retire its suppressed latch
        // even when no safe footprint exists; it cannot keep a hidden pet
        // polling, or replay when the selection is released.
        self.pet_hold_t = 0.0;
        self.pending_pet = 0;
        self.pet_at = None;
        let body = self.console_body(sense);
        let world = self.console.world.as_ref()?;
        let home = self.console_caret_home(sense, width);
        if !sense.reduced_motion && self.alpha > 0.0 && !Self::near_console_home(body, home) {
            // A moved caret has first claim on a visible resident. Let the
            // existing cursor follower carry it home, including its normal
            // row-hop path. A blocked direct perch corridor must not keep an
            // old output anchor parked on the other side of the console.
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            return None;
        }
        let mut target = self
            .console
            .target
            .filter(|r| Self::near_console_home(*r, home));
        if target.is_none_or(|r| !world.under_text_clear(r, CLEARANCE)) {
            target = if Self::near_console_home(body, home)
                && Self::console_may_stand(world, body, CLEARANCE)
            {
                Some(body)
            } else {
                world.home_perch(
                    home.unwrap_or(body),
                    CLEARANCE,
                    if home.is_some() {
                        HOME_REACH
                    } else {
                        PERCH_REACH
                    },
                )
            };
        }
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
        // A certificate for the current displayed body means stay here.
        // Converting that pixel rectangle back to fractional feet would
        // otherwise move a settled resident on its next idle observation.
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
        self.departures = [None; PET_DEPARTURES_MAX];
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
                    // The caret or selection just claimed the old cells.
                    // Keep presence intact: the final footprint resolver
                    // displaces the body to nearby protected-free ground.
                    // Zeroing the envelope here used to insert a blink before
                    // that resolver could act.
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
        self.set_action_keep(if self.console.moving {
            PetAction::Walk
        } else {
            PetAction::Sit
        });
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

    /// MAY THE PET BE WHERE IT NOW IS? Every console arm that asks this of
    /// the pet's CURRENT body asks it through here, and the answer never
    /// counts the caret's own keep-off ring.
    ///
    /// [`STATION_LEAD`] seats the escort one cell past the caret, inside
    /// that 3x3 ring by construction — that is the shipped escort law and
    /// what the pet looked like at v0.76.0. Read strictly, every one of
    /// these arms answers "no" for the escort's own station and reaches for
    /// a perch clear of the ring: the resident trips to a target two cells
    /// out and owes frames forever, placement relocates a walking cat, the
    /// repair inspection never engages. That is a bound on the reading
    /// returned as a fact about the place.
    ///
    /// CHOOSING a new station is the opposite question and keeps the strict
    /// reading (`home_perch`, `nearest_under_text_perch`, the rounding
    /// re-check in [`Self::place_console_body_at_home`]), so the pet can
    /// still never be PARKED on the cursor.
    fn console_may_stand(world: &PetWorld, rect: PetRect, margin: f32) -> bool {
        world.under_text_clearance_past_caret(rect, margin) == Some(true)
    }

    /// THE ESCORT'S OWN STATION, CORRECTED ONLY WHERE THE LADDER IS BLIND.
    ///
    /// This layer used to REPLACE the baseline station: it took the caret's
    /// desired column, ran it through [`Self::console_home_rect`] — which
    /// adds [`HOME_BREATHING_ROOM`] — and answered with a `home_perch`
    /// search around THAT, discarding the ladder's answer. Every station the
    /// escort chose was therefore two cells further from the caret than the
    /// shipped escort law puts it, on every frame, for the pet's whole life.
    /// That constant displacement is what "it no longer sits with me" was.
    ///
    /// The layer's real contribution is narrower, and it is kept: the ink
    /// ladder knows about GLYPHS and nothing else, so it can seat the escort
    /// on a selection, an image, or a row with no certified cell-to-pixel
    /// projection — places the pet must not stand. So this answers `Some`
    /// for exactly those, and `None` everywhere else, which hands the caret
    /// its escort back.
    ///
    /// Three things are deliberately NOT a reason to move the escort:
    ///
    ///  * THE CARET'S OWN KEEP-OFF RING. The pet is the caret's escort and
    ///    [`STATION_LEAD`] seats it one cell past the caret, inside that
    ///    ring by construction. Treating the ring as an obstruction is how
    ///    the standoff was paid twice over.
    ///  * ORDINARY INK. A resident may stand behind text — that is a
    ///    Z-ORDER fact (`frame.under_ink`), settled in 259649fe2, and the
    ///    ladder already owns the glyph rules the escort actually follows.
    ///  * AN UNKNOWN. `None` from the three-valued reading is a bound on
    ///    this map, never a claim about the terminal.
    ///
    /// And the correction searches with [`PetWorld::nearest_under_text_perch`],
    /// not `home_perch`: the escort must take the CLOSEST place it may
    /// stand, never travel to distant blank sky to avoid ink it is allowed
    /// to stand behind.
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
        // NO PERCH IN REACH IS NOT AN OPINION. This used to answer
        // `(self.col, self.row)` — "stay exactly where you are" — which is a
        // BOUND ON WHAT THIS LAYER KNOWS returned as a fact about where the
        // pet belongs, and it retired the caret escort outright: every
        // baseline station (`station_safe`'s ink ladder, the reduced-motion
        // pin) was replaced by the pet's own current position, so the pet
        // stopped following the caret at all. `None` is the honest answer,
        // and both callers already have the baseline to fall back to.
        let next = world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH)?;
        Some((next.col, next.foot_row()))
    }

    /// The body rect the escort's own station puts the pet in — the station
    /// exactly as the ladder chose it, with no breathing room added.
    /// [`Self::console_home_rect`] is the RESIDENT's home and keeps its
    /// buffer; this is the escort's stand and must not.
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

    fn console_caret_home(&self, sense: PetSense, width: f32) -> Option<PetRect> {
        sense.caret.or(self.console.cursor_home).map(|(r, c)| {
            // Side choice includes the same breathing room as placement.
            // Otherwise a nominal right station fits but its buffer hangs
            // off-screen, beyond reach of the valid left-side body.
            let left = f32::from(c) + STATION_LEAD + HOME_BREATHING_ROOM + width + CLEARANCE
                > f32::from(sense.cols);
            let col = if left {
                (f32::from(c) - width - STATION_LEAD).max(0.0)
            } else {
                f32::from(c) + STATION_LEAD
            };
            Self::console_home_rect((col, f32::from(r)), sense.rows, width, left)
        })
    }

    fn near_console_home(rect: PetRect, home: Option<PetRect>) -> bool {
        home.is_none_or(|home| {
            (rect.col - home.col)
                .abs()
                .max((rect.row - home.row).abs() * 2.0)
                <= HOME_REACH
        })
    }

    fn console_home_rect(desired: (f32, f32), rows: u16, width: f32, left: bool) -> PetRect {
        PetRect::new(
            (desired.1 + 1.0 - ART_ROWS).clamp(
                CLEARANCE,
                (f32::from(rows) - ART_ROWS - CLEARANCE).max(CLEARANCE),
            ),
            if left {
                (desired.0 - HOME_BREATHING_ROOM).max(CLEARANCE)
            } else {
                desired.0 + HOME_BREATHING_ROOM
            },
            ART_ROWS,
            width,
        )
    }

    /// Cursor home and protected cells constrain every emitted body, including
    /// a pose hold or flight. Resolve a moved caret with one bounded local
    /// placement, preserving the full body and alpha. Translate the existing
    /// motion coordinates together so the next tick cannot restore an old
    /// output perch. Ordinary ink permits residency; protected surfaces yield.
    fn place_console_body_at_home(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if self.console.clipped || frame.alpha == 0 {
            return;
        }
        let rect_of = |frame: PetFrame| {
            frame
                .body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
                .map(|(x0, x1, y0, y1)| {
                    PetRect::new(
                        y0 as f32 / f32::from(sense.cell_h.max(1)),
                        x0 as f32 / f32::from(sense.cell_w.max(1)),
                        (y1 - y0) as f32 / f32::from(sense.cell_h.max(1)),
                        (x1 - x0) as f32 / f32::from(sense.cell_w.max(1)),
                    )
                })
        };
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        let Some(rect) = rect_of(*frame) else { return };
        let home = self
            .console_caret_home(sense, rect.cols)
            .map(|home| PetRect {
                row: (home.foot_row() + 1.0 - rect.rows).clamp(
                    CLEARANCE,
                    (f32::from(sense.rows) - rect.rows - CLEARANCE).max(CLEARANCE),
                ),
                rows: rect.rows,
                ..home
            });
        // A BODY ALREADY WHERE IT MAY BE STAYS THERE, and the caret's own
        // keep-off ring is not a reason to move it. [`STATION_LEAD`] seats
        // the escort one cell past the caret — inside that 3x3 ring by
        // construction — so the strict reading refused the escort's own
        // station on every frame and relocated a WALKING cat to a perch
        // clear of the ring, two cells further out. That relocation, not
        // the search below it, is half of the standoff the owner reported.
        // Everything the strict reading actually protects — a selection, an
        // image, a row with no certified projection — still relocates here,
        // and the DESTINATION search below stays strict, so the pet can
        // still never be PARKED on the cursor.
        if Self::near_console_home(rect, home) && Self::console_may_stand(world, rect, 0.0) {
            return;
        }
        let Some(safe) =
            world.nearest_under_text_perch(home.unwrap_or(rect), CLEARANCE, HOME_REACH)
        else {
            self.console.clipped = true;
            self.console.reason = "protected-home-unavailable";
            return;
        };
        // Invert body_px's pixel projection instead of adding a delta to a
        // possibly viewport-clamped source. After a line wrap the raw flight
        // can be beyond the right edge: shifting its CLAMPED box left by dx
        // would leave the raw body several cells short of the certified spot.
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
        if rect_of(shifted).is_none_or(|r| !world.under_text_clear(r, 0.0)) {
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
            // The contact is a pose over the existing displacement. Flights
            // keep their own silhouette unless this is the console's trip.
            //
            // A SETTLED CAT'S OWN ANIMATION IS ITS OWN. `|| self.action
            // .settled()` used to stand here, and `settled()` is
            // Sleep|Sit|Loaf|Purr|Groom|Perk|Stand — so one keystroke, which
            // sets `console.pose = Some(PetStandEar)` and leaves `resident`
            // FALSE, pinned a sitting, loafing, grooming or sleeping cat
            // into a single stand-ear silhouette with `scale = 1` and
            // `purr = 0` for the whole of INPUT_HOLD, while its own action
            // still said Loaf. Measured on a Sit: one pose for 36 frames,
            // against a tail-flicking cat with the layer off. That is the
            // pet losing its own life the moment you type, and it is exactly
            // the state where this layer has the LEAST to say — the cat is
            // doing nothing that needs correcting.
            //
            // Contact stays: advancing ink running into the body is a real
            // located event with a 0.40 s hold, not a standing claim on the
            // silhouette, and it is one of the features this restoration
            // keeps.
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
            frame.departures = [None; PET_DEPARTURES_MAX];
        }
        self.place_console_body_at_home(frame, sense);
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        if world.stamp().is_none() {
            // NO CERTIFIED MAP AT ALL — not "I cannot see this rectangle" but
            // "I cannot see anything". The pet is drawn only over pixels this
            // observation certified, so an incoherent surface draws nothing.
            // That is the shipped rule and it stays: reflow, a mid-scroll
            // fractional offset and a stale snapshot all land here, and none
            // of them can vouch for the position the pet is standing in.
            //
            // It deliberately does NOT reach the visibility hold below. A
            // surface that went momentarily incoherent is not evidence of
            // INK, and must not spend the dwell that keeps a real
            // obstruction honest — nor may it clear `on_glass`, which would
            // hand the next tick's reseat a teleport it has not earned.
            frame.alpha = 0;
            frame.lane_alpha = 0;
            frame.motes = [None; PET_MOTES_MAX];
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.clipped = true;
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "incoherent-surface";
            return;
        }
        let body = frame
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
            });
        // THE BODY IS STILL EXACTLY WHERE IT IS, whatever the veto decides.
        // v0.81.0 cleared this on a vetoed frame; the next tick's
        // `reseat_unshown_console_body` then read the absence as "the
        // previous emission contained no body", TELEPORTED the pet to a new
        // station and set `alpha = 0`, restarting the 0.30 s fade. The veto
        // and the reseat retriggering each other is the self-sustaining
        // blink the owner reported. `on_glass`, set below, is the separate
        // and explicit fact the reseat actually wanted.
        if let Some(rect) = body {
            self.console.last_body = Some(rect);
        }
        // SELECTED TEXT SUBORDINATES THE PET AT ONCE, and is deliberately
        // NOT held: a selection is a user act, not a per-frame perception
        // wobble, and the whole point of selecting text is to read or copy
        // it right now. The union of the examined selected cells is a
        // conservative box — over-yielding here is the safe direction, and
        // selections do not change at the keystroke rate, so it cannot
        // strobe. (Shipped rule, carried by the resident-handoff and
        // landing-wake conformance suites, kept exactly.)
        let selected = body
            .zip(world.selection_rect())
            .is_some_and(|(b, s)| b.overlaps(s));
        if self.console.clipped || selected {
            // PLACEMENT ALREADY SEARCHED THE WHOLE REACH this tick and found
            // nowhere safe to stand — a dense screen. That is a SEARCHED
            // result, not a bound on perception, and the shipped rule (the
            // derived resident-handoff model carries it) is that it yields
            // at once. The envelope is moved to where the glass already is
            // rather than left to fight it.
            frame.alpha = 0;
            frame.lane_alpha = 0;
            self.console.hold.hide_now(sense.now);
            self.console.clipped = true;
            if !self.console.reason.starts_with("protected-") {
                self.console.reason = "protected-footprint";
            }
        } else {
            // THE VISIBILITY VERDICT, THREE-VALUED. `None` means "I cannot
            // see there" — a body outside the caret-centred coverage window,
            // which SLIDES AS THE USER TYPES, or no body to certify at all —
            // and it is NOT evidence of ink. The hold spends it as such: an
            // unknown keeps whatever verdict was already held.
            //
            // `under_text_clearance_past_caret`, and each half of that name
            // is one of the two sides this reconciliation had to keep:
            //
            //  * UNDER TEXT — ordinary ink in front of the body is a Z-ORDER
            //    fact, not an identity one, so it can no longer switch a
            //    resident off. `frame.under_ink` below is what the renderer
            //    does with it instead.
            //  * PAST CARET — PLACEMENT keeps the caret protected so the pet
            //    never comes to REST on the cursor, but a pet merely walking
            //    PAST the caret must not be deleted for the two frames it
            //    overlaps that one cell. The pet is the caret's ESCORT, and
            //    with the pet now HOMED on the caret (HOME_REACH) that ring
            //    is exactly where it lives.
            //
            // A frame with NO BODY goes to `idle`, not to `update`: the
            // emitter is drawing nothing of its own accord (an arrival fade
            // that has not started, no caret, a finished retirement), which
            // is neither evidence about the place the pet stands nor
            // blindness about it — there is no rectangle to be blind about.
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
                // THE FOOTPRINT IS REFUSED ONLY ONCE THE YIELD HAS COMPLETED.
                // While the ramp is still running the pet is genuinely on
                // glass and still owes frames, so `clipped` — which
                // `console_frames` reads as "nothing further to draw" — must
                // not be latched until the cover has actually reached zero.
                self.console.clipped |= frame.alpha == 0;
            }
            // ORDINARY TERMINAL INK IS IN FRONT OF THE FULL BODY. A busy
            // prompt must not switch a resident off or shrink it — it
            // changes the z-order and nothing else. The strict (two-valued)
            // reading is the right one to ask here: an unobserved rectangle
            // answers `false` and draws under the text, which is the safe
            // direction for a layering decision.
            if body.is_some_and(|rect| !world.clear(rect, 0.0)) {
                frame.under_ink = true;
            }
        }
        if frame.alpha == 0 {
            frame.motes = [None; PET_MOTES_MAX];
            frame.departures = [None; PET_DEPARTURES_MAX];
        }
        // `clipped` is set by the two branches above, which are the only ones
        // that REFUSE a footprint. It is deliberately NOT set here: a frame
        // can be transparent because the pet is still FADING IN — its own
        // 0.30 s arrival ramp, or a first capture — and latching the refusal
        // on that turned the very next frame, with a certified-clear verdict
        // under it, into a permanent blank.
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
        let base = *frame;
        for departure in &mut frame.departures {
            if departure.is_some_and(|d| {
                let ghost = PetFrame {
                    alpha: d.alpha,
                    col: d.col,
                    row: d.row,
                    lift: 0.0,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    ..base
                };
                ghost
                    .body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
                    .is_none_or(|(x0, x1, y0, y1)| {
                        !world.clear(
                            PetRect::new(
                                y0 as f32 / f32::from(sense.cell_h.max(1)),
                                x0 as f32 / f32::from(sense.cell_w.max(1)),
                                (y1 - y0) as f32 / f32::from(sense.cell_h.max(1)),
                                (x1 - x0) as f32 / f32::from(sense.cell_w.max(1)),
                            ),
                            CLEARANCE,
                        )
                    })
            }) {
                *departure = None;
            }
        }
    }

    pub(super) fn console_frames(&self) -> Option<bool> {
        if !self.console.presentable || self.console_legacy_motion_pending() {
            return None;
        }
        // A visibility hold in transit is a FADE ON GLASS and a dwell still
        // counting: it owes frames whatever else the pet is doing, or the
        // ramp stalls until some unrelated repaint happens to tick it.
        if !self.console.hold.settled() {
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
        // THE SPENT PERCH'S OWN EDGE. Without this the resident that has
        // nothing left to react to offers nothing at all, and the release
        // below can never be reached — the dwell would be a timer nothing
        // ticks. Finite by construction: one dwell per perch.
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

/// Compare occupied ink at the SAME body rectangle before/after observation.
/// Moving a cat or a caret toward old ink cannot manufacture contact. This
/// tiny independent mask includes at most 16×32 cells; overflow is no claim.
fn local_ink(world: &PetWorld, body: PetRect) -> Option<[u64; 8]> {
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
                Some(true) => {
                    let i = (r - r0) * 32 + c - c0;
                    bits[i / 64] |= 1 << (i % 64);
                }
                Some(false) => {}
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
    fn repair_holds_its_body_and_only_an_attributed_range_licenses_the_paw() {
        let mut s = Scene::new();
        s.input(PetInputKind::Delete);
        s.term.process(b"\x1b[6;15H");
        let inspect = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Editing);
        assert_eq!(inspect.pose, PetGlyphId::PetInspectDown);
        let before = (inspect.col, inspect.row);
        s.frame(0.016);
        assert_eq!((s.pet.col, s.pet.row), before);
        let stamp = s.pet.console.world.as_ref().unwrap().stamp().unwrap();
        let range = PetRect::new(5.0, 17.0, 1.0, 1.0);
        let seq = s.pet.console_input_seq();
        for invalid in [
            PetRect::new(f32::NAN, 17.0, 1.0, 1.0),
            PetRect::new(5.0, 1000.0, 1.0, 1.0),
        ] {
            assert!(
                !s.pet
                    .note_console_edit(seq, stamp, invalid, PetEditPhase::Replaced)
            );
        }
        assert!(
            !s.pet
                .note_console_edit(seq + 1, stamp, range, PetEditPhase::Vacated)
        );
        assert!(
            s.pet
                .note_console_edit(seq, stamp, range, PetEditPhase::Vacated)
        );
        assert_eq!(s.frame(0.016).pose, PetGlyphId::PetReachPaw);
        s.term.process(b"\x1b[6;18Hr\x1b[6;15H");
        let f = s.frame(0.016);
        assert_ne!(
            f.pose,
            PetGlyphId::PetReachPaw,
            "replacement ink revokes vacancy witness"
        );
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
        let mut s = Scene::new();
        let input = s.term.cell_frame(20, 80);
        s.term.process(b"changed");
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.now += Duration::from_millis(16);
        let f = s.pet.tick(PetSense {
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
        });
        assert_eq!(f.alpha, 0);
        assert_eq!(f.lane_alpha, 0);
        assert_eq!(s.pet.console_reason(), "incoherent-surface");
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

    /// THE FROZEN DECAL. A content perch taken after a command finished has
    /// no live event left to react to, yet `console_frames()` answers
    /// `Some(false)` and `console_deadline()` answers `None`: the resident is
    /// left with NO FUTURE AT ALL — no frame train, and no wake instant
    /// either — so it can never breathe, blink, settle or sleep, and it can
    /// never notice that the caret has walked away. A pet that cannot be
    /// ticked again is a decal.
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
        // Let the perch trip finish, then hold the screen perfectly still.
        for _ in 0..40 {
            s.frame(0.016);
        }
        assert!(
            s.pet.needs_frames() || s.pet.next_change_deadline(s.now).is_some(),
            "a resident with nothing left to react to must still own a future"
        );
        // …and that future is its own life: the perch is spent, so the pet
        // goes back to escorting the caret and running the idle ladder.
        for _ in 0..(3 * 62) {
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

    /// THE ESCORT. When no perch is in reach the console layer knows nothing
    /// about where the pet belongs — that is not the same as "stay exactly
    /// where you are", which is what it used to answer, and which stops the
    /// pet following the caret at all.
    ///
    /// THE FIXTURE IS DOUBLE-WIDTH LINES, not dense ink, and the change is
    /// part of this reconciliation. A screen packed with `X` used to offer
    /// nowhere to stand; a resident may now stand UNDER ordinary ink
    /// (`under_text_clear`), so dense text is no longer a screen with no
    /// perch on it. DECDWL is: a non-single-width line has no certified
    /// cell-to-pixel projection, so every cell is `Protected` and the
    /// under-text reading refuses it too. The subject of the test is
    /// unchanged — what `console_station` answers when the search genuinely
    /// comes back empty.
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
}
