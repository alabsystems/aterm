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
    contact_changed: bool,
    observed_seq: u64,
    progress_seq: u64,
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

    pub(super) fn console_obstructed(&self, sense: PetSense) -> bool {
        self.console.presentable
            && self.console.world.as_ref().is_some_and(|world| {
                world.stamp().is_some() && !world.clear(self.console_body(sense), CLEARANCE)
            })
    }

    pub(super) fn reseat_unshown_console_body(&mut self, sense: PetSense, width: f32) {
        // The previous emission contained no body. A first appearance or a
        // clearance-hidden resident may therefore start its finite fade at a
        // current safe station; a still-visible body must use locomotion.
        if self.console.last_body.is_some()
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
            }
        }
    }

    /// Read once per brain tick, before choosing between caret work and a
    /// located reading/output interest. No host classifies pet behavior.
    pub(super) fn begin_console_tick(&mut self, sense: PetSense, width: f32) {
        let contact_changed = core::mem::take(&mut self.console.contact_changed);
        self.console.pose = None;
        self.console.still = false;
        self.console.lift = 0.0;
        self.console.clipped = false;
        self.console.attention = PetAttention::Rest;
        self.console.reason = "quiet";
        let body = self.console_body(sense);
        let Some(stamp) = self.console.world.as_ref().and_then(|w| w.stamp()) else {
            self.console.resident = false;
            self.console.trip = None;
            return;
        };
        let prior = self.console.tick_stamp.replace(stamp);
        let same_surface = prior.is_some_and(|p| p.surface == stamp.surface);
        let new_content = same_surface && prior.is_some_and(|p| p.content_seq != stamp.content_seq);
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
        let selection = world.selection_target();
        // Surface protection outranks everything. A new key still cancels the
        // old trip, but cannot license a performance over a held selection.
        if let Some(gaze) = selection {
            self.console.attention = PetAttention::Reading;
            self.console.reason = "selection";
            self.console.source_seq = self.console.observed_seq;
            self.console.gaze = Some(gaze);
            self.console.anchor = None;
            self.console.target = if world.clear(body, CLEARANCE) {
                Some(body)
            } else {
                world.perch_near(gaze, body, CLEARANCE, PERCH_REACH)
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
            if repairing && world.clear(body, CLEARANCE) && self.flight.is_none() {
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
            let safe = world.clear(body, CLEARANCE);
            let roomy = world.clear(body, CONTACT_MARGIN);
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
            self.choose_console_perch(subject, body, stamp, prior);
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
            self.console.target = world.clear(body, CLEARANCE).then_some(body);
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
            self.console.attention = PetAttention::Exploring;
            self.console.reason = "content-perch";
            self.console.gaze = Some((anchor.row, anchor.col));
            self.console.pose = Some(PetGlyphId::PetEdgePerch);
            self.choose_console_perch(anchor, body, stamp, prior);
            self.console.resident = true;
            self.console.still = true;
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
            if world.clear(target, CLEARANCE) && !leaving_view {
                self.console.anchor = Some(anchor);
                self.console.target = Some(target);
                return;
            }
        }
        // Aim just beyond the coalesced output edge, not at every new line.
        let next = world.perch_near(
            (subject.row, subject.col + 2.0),
            body,
            CLEARANCE,
            PERCH_REACH,
        );
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
        let mut target = self.console.target;
        if target.is_none_or(|r| !world.clear(r, CLEARANCE)) {
            target = if world.clear(body, CLEARANCE) {
                Some(body)
            } else {
                world.nearest_perch(body, CLEARANCE, PERCH_REACH)
            };
        }
        let Some(target) = target else {
            self.console.clipped = true;
            self.console.moving = false;
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "no-clear-footprint";
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
            if !world.corridor_clear(body, target, CLEARANCE) {
                // Watching from here is enough; a refused trip is never retried
                // by an idle timer. New displayed facts may choose another.
                self.console.trip = None;
                if world.clear(body, CLEARANCE) {
                    self.console.target = Some(body);
                } else {
                    // The old body was consumed by output or left the
                    // viewport. Emit one hidden frame, then let the existing
                    // finite fade place it at this certified destination.
                    // Alpha zero supplies the next wake; a dense map with no
                    // destination takes the quiet no-target branch instead.
                    self.console.target = Some(target);
                    self.alpha = 0.0;
                    self.console.reason = "perch-reentry";
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
                self.console.lift = if trip.hop && world.clear(arc, CLEARANCE) {
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
        let body = PetRect::new(
            (desired.1 + 1.0 - ART_ROWS).max(CLEARANCE),
            desired.0,
            ART_ROWS,
            width,
        );
        let next = world.nearest_perch(body, CLEARANCE, PERCH_REACH);
        let _ = sense_dims;
        Some(next.map_or((self.col, self.row), |r| (r.col, r.foot_row())))
    }

    pub(super) fn finish_console_frame(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if !self.console.presentable {
            return;
        }
        if let Some(pose) = self.console.pose {
            // The contact is a pose over the existing displacement. Flights
            // keep their own silhouette unless this is the console's trip.
            if !self.console_legacy_motion_pending()
                && self.action != PetAction::Land
                && (self.console.resident
                    || self.console.attention == PetAttention::Contact
                    || self.action.settled())
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
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        if world.stamp().is_none() {
            frame.alpha = 0;
            frame.lane_alpha = 0;
            frame.motes = [None; PET_MOTES_MAX];
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.clipped = true;
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "incoherent-surface";
            return;
        }
        if let Some((x0, x1, y0, y1)) =
            frame.body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
        {
            let cw = f32::from(sense.cell_w.max(1));
            let ch = f32::from(sense.cell_h.max(1));
            let rect = PetRect::new(
                y0 as f32 / ch,
                x0 as f32 / cw,
                (y1 - y0) as f32 / ch,
                (x1 - x0) as f32 / cw,
            );
            self.console.last_body = Some(rect);
            if self.console.clipped || !world.clear(rect, 0.0) {
                frame.alpha = 0;
                frame.lane_alpha = 0;
                frame.motes = [None; PET_MOTES_MAX];
                frame.departures = [None; PET_DEPARTURES_MAX];
                self.console.clipped = true;
                self.console.last_body = None;
            }
        }
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
        if self.console.resident
            && self.console.presentable
            && !self.console_legacy_motion_pending()
        {
            Some(
                self.console.resident_handoff
                    || self.console.completion.is_some()
                    || self.console.seq != self.console.consumed
                    || self.pending_pet > 0
                    || (!self.console.clipped && (self.console.moving || self.alpha < 1.0)),
            )
        } else {
            None
        }
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
        [
            input,
            contact_recover,
            contact,
            result,
            progress,
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
}
