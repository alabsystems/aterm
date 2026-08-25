// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESS CUSTODY — the RECORD of which custody transition last fired.
//!
//! Custody is about two things the user owns: the reading position (the grid's
//! `display_offset`) and the highlight (`TextSelection`). A dozen different events can
//! move one or both of them, and until this module existed the engine kept no trace
//! of WHICH one did. That had two costs.
//!
//! The user-facing cost is the whole reason this design exists: "my selection
//! disappeared and I do not know why". Offset and selection are observable after the
//! fact; the DECISION that moved them is not. `custody` on the control socket answers
//! it directly — the last transition, by name, beside the state it left behind.
//!
//! The verification cost is that several of those events are indistinguishable in
//! every observable variable. An auto-repeat tick, a bare modifier and a key release
//! all leave offset, ownership and the selection exactly as they found them, and all
//! three arrive at `apply_press_custody` as the single value `disturbs == false`. A
//! bool is a two-way channel for a four-way fact, so a conformance that had to INFER
//! the event from the state change could never tell the three apart, and an invariant
//! about "a repeat is inert" would be satisfied by construction rather than checked.
//! The record carries the discriminator instead of inferring it.
//!
//! COST. Two `Option<CustodyTransition>` fields on [`Terminal`] — a fieldless enum, so
//! each record is ONE BYTE and both land in existing struct padding. Recording is a
//! store plus one compare, with no allocation and no clock read, and every recording
//! site already holds the terminal mutably (they are all mid-mutation), so the record
//! adds no lock traffic of its own on the press path, the mouse path or the output
//! path. Two sites DID have to start taking a lock they previously skipped — the key
//! RELEASE arm of the input seam and the hidden-session repeat path — and both say so
//! at the call site.
//!
//! WHY TWO. `last_custody` is the LAST event, full stop, and the Tier-1 conformance
//! needs exactly that: it arms the slot and requires the very next seam to fill it.
//! A human asking "why did my selection disappear?" needs something else. The most
//! frequent writer by orders of magnitude is [`CustodyTransition::OutputAtLive`] — a
//! shell prompt, a `cat`, a `tail -f` — and every one of those is a no-op that took
//! nothing, so a single slot answers a `custody` typed one second after the fact with
//! the last line of shell output rather than with the event being asked about.
//! `last_custody_change` is the same byte latched only when the event really TOOK
//! something: the offset moved, or a live highlight died. The `custody` verb prints
//! both (`last=` and `changed=`), so the raw sequence stays visible and the question
//! the record exists for has an answer that survives the next prompt.
//!
//! WHAT A RECORD IS NOT. It is not a self-reported "this press disturbed something"
//! flag. `PressCustody`'s invariants are stated over the OBSERVABLE state (offset,
//! owner, selection) against a shadow of the pre-action values; the recorded name
//! only says WHICH action to hold the step to. An implementation that moved the
//! viewport on a repeat and still recorded [`CustodyTransition::RepeatPress`] fails
//! `RepeatPressIsInert` — recording the name it wanted does not help it.

use super::state::Terminal;

/// Which custody transition last fired — one variant per `PressCustody` action, plus
/// one for the shape the model has no action for
/// ([`Self::OutputTookTheSelectionUnattributed`]).
///
/// The three inert press classes are separate variants even though they are
/// state-identical, because they are separate FACTS: a release, an auto-repeat tick
/// of a hold in progress, and a bare modifier are three different reasons for the
/// viewport to have stayed put, and a regression in one of them is invisible if they
/// share a name.
///
/// PRIORITY. The press bits OVERLAP — `is_modifier_or_lock_key` matches on the KEY
/// alone with no event-type term, so a Shift key-up sets "release" and "inert
/// modifier" at once, and a held Shift's auto-repeat sets "repeat" and "inert
/// modifier" at once. The model's variants are disjoint, so the press seam resolves
/// them in the fixed order Release > Repeat > Inert > Typing (see
/// `aterm_gui::app_input::PressKind`). Any other order re-labels the same physical
/// event without changing whether it disturbs — a trace corruption a passing model
/// check would never catch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CustodyTransition {
    /// The user moved the viewport back into history (a wheel notch, PgUp, the
    /// `scroll` verb, or a selection drag past the grid edge). Only a move that
    /// RAISES the offset is this transition; scrolling back down toward live is not
    /// a `PressCustody` action at all and records nothing.
    UserScroll,
    /// A user gesture that leaves a selection behind — the press that starts a drag,
    /// and the release that completes one.
    UserSelect,
    /// A user gesture that deliberately deselects: a press and release inside one
    /// cell. Always allowed; it is the user's own highlight.
    UserClear,
    /// The ONE handover: a byte-producing press means "take me back to the prompt",
    /// so the viewport snaps to live and the selection goes.
    TypingPress,
    /// An auto-repeat tick of a hold whose press already took custody. Inert —
    /// re-running the snap at the ~30 Hz repeat rate is what destroyed scrolls and
    /// selections made mid-hold.
    RepeatPress,
    /// A bare modifier or lock key (Shift/Control/Alt/Super/Caps…). Inert — this is
    /// the first half of Cmd-C, and clearing the selection here is what made Cmd-C
    /// copy nothing.
    InertPress,
    /// A key RELEASE report (Kitty `REPORT_EVENT_TYPES`). Inert — a key-up is not
    /// typing.
    ReleaseEvent,
    /// Program output arrived while the tail-follower owned the viewport. The view
    /// stays at live because it already was; the content advances underneath it.
    OutputAtLive,
    /// Program output arrived while the USER owned the viewport. The re-pin rides
    /// the arriving lines so the same content stays under the eye — the offset
    /// RISES, and ownership stays with the user.
    OutputWhileReading,
    /// Program output REPLACED the rows the selection was sitting on. The highlight
    /// dies (a highlight left over replaced text makes a copy return something the
    /// user never selected) — but the reading position is still not output's to
    /// take.
    OutputDamagesTheSelectedRows,
    /// ED 3 / `clear_scrollback` / RIS: the coordinate space the offset and the
    /// selection anchors were stated in is gone, so the viewport goes back to the
    /// tail-follower and the selection cannot outlive the rows it named.
    OutputInvalidatesTheCoordinateSpace,
    /// Output took a live highlight for a reason that is NOT damage overlap, and the
    /// model has no action for it. NOT a `PressCustody` action — [`Self::action`]
    /// returns a name no model admits and [`Self::last_event`] returns `-1`, outside
    /// the model's `0..=7` tag space, so a step carrying it can never be validated as
    /// if it were one.
    ///
    /// `post_process` has five ways to reach this: a malformed or saturated splice
    /// projection, a splice mixed with another scroll in one batch (the historical
    /// fail-closed arm), an alt-screen exit mid-batch, the `left_alt` upper-bound
    /// re-check, and a whole-interval eviction at the history floor.
    ///
    /// This variant exists because those five are precisely the flagship complaint —
    /// a selection vanishing with no key press to blame — and the record used to
    /// answer them by EMPTYING itself, which prints `last=none`: indistinguishable
    /// from a terminal that has never done anything, and it destroyed whatever true
    /// record was standing on the way out. "Output took it, for a reason I cannot
    /// name" is a real answer, and it rules out the keyboard.
    OutputTookTheSelectionUnattributed,
}

impl CustodyTransition {
    /// The `PressCustody` action this transition IS, by the model's own spelling.
    ///
    /// Used by the Tier-1 conformance to name the action it validates the observed
    /// step against, so the ACTION NAME comes from the engine's own classification
    /// rather than from the harness's expectation.
    #[must_use]
    pub fn action(self) -> &'static str {
        match self {
            Self::UserScroll => "UserScroll",
            Self::UserSelect => "UserSelect",
            Self::UserClear => "UserClear",
            Self::TypingPress => "TypingPress",
            Self::RepeatPress => "RepeatPress",
            Self::InertPress => "InertPress",
            Self::ReleaseEvent => "ReleaseEvent",
            Self::OutputAtLive => "OutputAtLive",
            Self::OutputWhileReading => "OutputWhileReading",
            Self::OutputDamagesTheSelectedRows => "OutputDamagesTheSelectedRows",
            Self::OutputInvalidatesTheCoordinateSpace => "OutputInvalidatesTheCoordinateSpace",
            // Deliberately NOT a model action: `Model::successors` panics on an
            // unknown action name, so a conformance that ever handed this step to
            // `validate_transition` fails loudly instead of quietly matching some
            // neighbouring action.
            Self::OutputTookTheSelectionUnattributed => "OutputTookTheSelectionUnattributed",
        }
    }

    /// The model's `last_event` tag: 0 a user gesture, 1 typing, 2 auto-repeat,
    /// 3 a bare modifier, 4 a release, 5 output that missed the selected rows,
    /// 6 output that REPLACED them, 7 output that invalidated the coordinate space.
    ///
    /// Note that the tag is deliberately NOT injective: the three user gestures all
    /// tag 0 and both undamaged-output kinds tag 5, exactly as the model declares.
    /// The VARIANT is the discriminator; the tag is the model's projection of it.
    #[must_use]
    pub fn last_event(self) -> i64 {
        match self {
            Self::UserScroll | Self::UserSelect | Self::UserClear => 0,
            Self::TypingPress => 1,
            Self::RepeatPress => 2,
            Self::InertPress => 3,
            Self::ReleaseEvent => 4,
            Self::OutputAtLive | Self::OutputWhileReading => 5,
            Self::OutputDamagesTheSelectedRows => 6,
            Self::OutputInvalidatesTheCoordinateSpace => 7,
            // OUTSIDE the model's tag space on purpose (`StateBounds` bounds it
            // `last_event <= 7`, and every invariant's else-arm reads `last_event <= 7`
            // as its trivial case). The `custody` verb prints `-` for it.
            Self::OutputTookTheSelectionUnattributed => -1,
        }
    }

    /// Whether this transition, at every site that records it, has already TAKEN
    /// something from the user — the latch condition for `last_custody_change`.
    ///
    /// True by the recording condition, not by hope. [`Self::UserScroll`] is recorded
    /// only on a RISE, [`Self::OutputWhileReading`] only when the re-pin really moved
    /// the offset, and the two destroying output kinds only when a highlight died.
    /// The four press classes are false because three of them are inert by law and the
    /// fourth ([`Self::TypingPress`]) may land on an already-live, unselected viewport
    /// and take nothing — `apply_press_custody` promotes it explicitly on the presses
    /// that really did snap or clear. [`Self::OutputAtLive`] is false because it is an
    /// identity transition: it is the record's most frequent writer and never its most
    /// interesting one.
    ///
    /// The output rows are DEFAULTS only: [`Terminal::note_output_custody`] does not
    /// go through them, because it can see the pre-batch offset, the post-re-pin
    /// offset and the selection on both sides of the batch and therefore computes the
    /// exact answer ([`Terminal::note_custody_at`]). An ED 3 typed at a live,
    /// unselected prompt takes nothing, and the exact form says so.
    ///
    /// STATED IMPRECISION: [`Self::UserClear`] latches even for a deselecting click
    /// made when nothing was selected. It is still the last gesture in which the user
    /// expressed an intent about the highlight, and the alternative — threading a
    /// pre-state bool through the gesture seam — buys a distinction no one asking
    /// "why did my selection disappear?" can act on.
    #[must_use]
    pub fn always_takes_custody(self) -> bool {
        match self {
            Self::UserScroll
            | Self::UserSelect
            | Self::UserClear
            | Self::OutputWhileReading
            | Self::OutputDamagesTheSelectedRows
            | Self::OutputInvalidatesTheCoordinateSpace
            | Self::OutputTookTheSelectionUnattributed => true,
            Self::TypingPress
            | Self::RepeatPress
            | Self::InertPress
            | Self::ReleaseEvent
            | Self::OutputAtLive => false,
        }
    }

    /// Did this transition destroy a live highlight WITHOUT the user asking?
    ///
    /// The `custody` verb exists to answer "why did my selection disappear?", and the
    /// honest answer is never "you deselected it" — the user knows that. These are the
    /// four that take a highlight the user did not release: output that replaced the
    /// selected rows, output that destroyed the coordinate space, output that took it
    /// for a reason the damage lattice could not attribute, and typing, which takes it
    /// as a documented side effect people still find surprising.
    #[must_use]
    pub fn takes_a_selection_the_user_did_not_release(self) -> bool {
        matches!(
            self,
            Self::OutputDamagesTheSelectedRows
                | Self::OutputInvalidatesTheCoordinateSpace
                | Self::OutputTookTheSelectionUnattributed
                | Self::TypingPress
        )
    }
}

/// How this output batch's `SelectionDamage` landed on the live selection — the
/// classification `post_process` can make and nothing else in the output path can.
///
/// Split from the bool `SelectionDamage::clears_selection` returns because that bool
/// CONFLATES the two destroying kinds: it short-circuits `All` to `true` without ever
/// calling the overlap predicate, so a recorder keyed off it alone would report
/// [`CustodyTransition::OutputDamagesTheSelectedRows`] for every ED 3 / RIS and the
/// invalidation transition would never appear in any trace.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum OutputDamage {
    /// No damage was recorded, or the batch recorded bands that MISSED the selection.
    /// Both leave the highlight alone, so they are one class here; `Missed` is kept
    /// separate only so the "a band was recorded and missed" case is distinguishable
    /// from "no band was recorded at all" by a future diagnostic.
    #[default]
    None,
    /// Bands were recorded and none of them intersected the selection.
    Missed,
    /// A recorded band intersected the selection, which is now gone.
    Hit,
    /// `SelectionDamage::All` — the coordinate space itself was destroyed.
    All,
}

impl Terminal {
    /// Record which custody transition just fired, and whether it actually TOOK
    /// something. One byte stored, one byte conditionally latched.
    ///
    /// Called from the site that MADE the decision, while it still holds the facts
    /// that separate the variants — never inferred later from the state change,
    /// because several variants leave identical state (see the module header).
    ///
    /// `took` is the site's own observation that custody moved: the offset changed,
    /// or a live highlight died. It is a separate argument rather than a re-read of
    /// the terminal because the deciding site is the only place both sides of the
    /// change are in hand — by the time anyone else looks, the "before" is gone,
    /// which is the same reason the record exists at all.
    #[inline]
    pub fn note_custody_at(&mut self, transition: CustodyTransition, took: bool) {
        self.last_custody = Some(transition);
        if took {
            self.last_custody_change = Some(transition);
        }
        // …and the one the verb is JUSTIFIED by, latched separately because
        // `last_custody_change` cannot answer it. That latch means "the last thing
        // that moved custody", so an ordinary left-click — a `UserClear`, which really
        // does move custody — overwrites it, and "why did my highlight vanish?" then
        // answers "you cleared it": true of the click, useless about the loss, and the
        // evidence is gone. A deliberate release is a cause the user already knows;
        // only an INVOLUNTARY taker is worth preserving across later activity.
        if transition.takes_a_selection_the_user_did_not_release() {
            self.last_selection_taker = Some(transition);
        }
    }

    /// [`Self::note_custody_at`] for the sites whose recording CONDITION already
    /// decides the answer — see [`CustodyTransition::always_takes_custody`].
    #[inline]
    pub fn note_custody(&mut self, transition: CustodyTransition) {
        self.note_custody_at(transition, transition.always_takes_custody());
    }

    /// Latch `last_custody_change` ONLY, leaving the raw last-event record alone.
    ///
    /// For a site that records BEFORE it acts and only afterwards learns whether it
    /// took anything — `apply_press_custody`, which must stamp the press class while
    /// it still has it and can only then discover whether the viewport was scrolled
    /// back or a highlight was alive. Deliberately not [`Self::note_custody_at`]: that
    /// would re-store `last_custody`, and a promotion is not a second event. (It also
    /// keeps a MUTANT honest — a press class that started disturbing must fail on the
    /// transition it really is, not be laundered into whichever name the promotion
    /// carried.)
    #[inline]
    pub fn note_custody_took(&mut self, transition: CustodyTransition) {
        self.last_custody_change = Some(transition);
    }

    /// The last recorded custody transition, or `None` if nothing has moved custody
    /// since this terminal was created.
    ///
    /// Non-consuming: this is the read the `custody` control verb makes, and asking
    /// twice must give the same answer.
    #[must_use]
    #[inline]
    pub fn last_custody_transition(&self) -> Option<CustodyTransition> {
        self.last_custody
    }

    /// The last recorded transition that actually TOOK the reading position or the
    /// highlight — the `changed=` half of the `custody` verb.
    ///
    /// Survives the ordinary output that overwrites [`Self::last_custody_transition`]
    /// thousands of times a second, which is what makes the verb usable by a human
    /// rather than only by a harness reading back inside the same step.
    #[must_use]
    #[inline]
    pub fn last_custody_change(&self) -> Option<CustodyTransition> {
        self.last_custody_change
    }

    /// The last transition that took a live highlight the user did NOT release.
    ///
    /// Survives later activity on purpose — clicking, scrolling or typing after the
    /// loss must not erase the explanation of it. Non-consuming, like its siblings.
    #[must_use]
    #[inline]
    pub fn last_selection_taker(&self) -> Option<CustodyTransition> {
        self.last_selection_taker
    }

    /// PRESS CUSTODY — the viewport half of the record: the user moved the reading
    /// position back into history.
    ///
    /// `before` is the `display_offset` sampled immediately before the move, inside
    /// the same `&mut self` borrow that performs it. Only a RISE is
    /// [`CustodyTransition::UserScroll`]: the model's action is guarded
    /// `offset <= MaxOffset - 1` and assigns `offset = offset + 1`, so it describes
    /// moving AWAY from live and nothing else. Scrolling back DOWN —
    /// `ScrollIntent::Down`, `Bottom`, a negative `By(n)`, the downward half of a
    /// selection autoscroll, the paste/IME snap — lowers the offset, and no
    /// `PressCustody` action admits that shape. Those record NOTHING rather than
    /// being labelled with the nearest action that fits: a mislabelled no-op is a
    /// corrupt trace, and `validate_transition` would accept one.
    ///
    /// WHY IT LIVES HERE AND NOT IN THE GUI. It used to be `app_input::
    /// note_scroll_custody`, called from two of the roughly eight seams that raise
    /// the offset — and the WHEEL, the dominant gesture and the first one this
    /// transition's own doc names, was not among them. `input_wheel`'s instant arm,
    /// its glide tick, `settle_scroll_motion_at_target`, `control::apply_scroll_intent`,
    /// the cross-session `mouse` wheel fallback and the search jump all reached
    /// [`Terminal::scroll_display`] / [`Terminal::scroll_to_absolute_row`] directly,
    /// so after a wheel notch the `custody` verb reported whatever press came before
    /// it beside a reading position that press had not moved. Recording in the three
    /// `Terminal` primitives that can raise the offset is bypass-proof by
    /// construction: there is no route to a higher `display_offset` that does not
    /// pass through one of them.
    ///
    /// One handle still reaches past it: `Terminal::grid_mut().scroll_display(..)`,
    /// the raw grid. Every caller of that form in the workspace is a test fixture or a
    /// bench harness (`aterm-gui::bench_support`, `aterm-bench`'s scroll-scrub
    /// example, and unit tests in `content.rs` / `state.rs` / `search_index.rs`); no
    /// shipping input path uses it, and a fixture that sets up an offset without
    /// claiming a user gesture is the honest use of it. Said out loud rather than
    /// left as an implied "no route exists".
    ///
    /// It is also clean with respect to the two documented NON-actions.
    /// [`Terminal::scroll_to_bottom`] can only lower the offset, so it never records
    /// — which is what keeps `apply_press_custody`'s snap from filing a second
    /// `UserScroll` on top of its own `TypingPress`. And the SCR-1 output re-pin goes
    /// through `Grid::repin_display_offset`, a grid-level entry point this method
    /// cannot see, so output that rides the arriving lines can never be misfiled as a
    /// user gesture.
    ///
    /// GRANULARITY, stated rather than hidden: the model moves the offset by exactly
    /// one, while a real notch moves `wheel_viewport_lines` rows and a real PgUp moves
    /// a whole page. The record says "the user took the viewport", not how far; Tier-1
    /// drives the one-row forms so the projection is the identity and no saturation is
    /// involved, and drives a real wheel notch for the RECORD alone.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PressCustody",
            action = "UserScroll",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[inline]
    pub(crate) fn note_scroll_custody(&mut self, before: usize) {
        if self.grid.display_offset() > before {
            self.note_custody(CustodyTransition::UserScroll);
        }
    }

    /// Take the last recorded custody transition, leaving `None` behind.
    ///
    /// The Tier-1 conformance uses this and not [`Self::last_custody_transition`]:
    /// clearing before a step and requiring `Some(_)` after it proves the step's site
    /// actually RECORDED, instead of the harness reading a stale tag left by an
    /// earlier step that happens to carry the name it expected.
    #[inline]
    pub fn take_custody_transition(&mut self) -> Option<CustodyTransition> {
        self.last_custody.take()
    }

    /// The output half of the record — the EMIT point of the three-site protocol
    /// `process_at` runs, and the one moment at which the pre-batch reading position,
    /// the damage verdict and the FINAL post-re-pin offset are all simultaneously
    /// true.
    ///
    /// The three sites and why it takes three:
    ///
    /// 1. The SCR-1 prologue latches `pinned_offset` — the pre-batch `display_offset`
    ///    — because the very next statement forces it to 0 for the duration of the
    ///    batch. That value is the ONLY thing in the process that separates
    ///    [`CustodyTransition::OutputAtLive`] from
    ///    [`CustodyTransition::OutputWhileReading`], and by `post_process` it is
    ///    already gone.
    /// 2. `post_process` classifies the damage into an [`OutputDamage`], which is the
    ///    only thing that separates a batch that merely scrolled from one that
    ///    replaced the selected rows or destroyed the coordinate space. It cannot
    ///    emit: the SCR-1 re-pin has not run yet, so the offset it can read is 0 for
    ///    every batch.
    /// 3. THIS, after the re-pin, where the final offset is readable — and it IS
    ///    read (`self.grid.display_offset()` below), not merely available. The
    ///    pre-batch offset alone cannot tell `OutputWhileReading` from a re-pin that
    ///    saturated: a user parked at the top of a FULL scrollback takes a line of
    ///    output, the new row evicts the oldest, `scrollback_lines()` is unchanged,
    ///    the re-pin target `pinned_offset + 1` clamps straight back to
    ///    `pinned_offset`, and the offset does not rise at all. The model's
    ///    `OutputWhileReading` MANDATES the rise, so recording one there would be a
    ///    step the spec rejects — for an ordinary `tail -f` against a full history.
    ///
    /// `lines_added` is what separates "output arrived" from "nothing happened": a
    /// batch of cursor moves or an OSC query advances no rows and matches no model
    /// action, so it records nothing rather than fabricating an `OutputAtLive` step.
    /// Damage is exempt from that test — ED 3 advances no rows and is still very much
    /// an event.
    ///
    /// `selection_before` catches the batches that destroyed a selection for a reason
    /// that is NOT damage overlap. `post_process` has five of them — a malformed or
    /// saturated splice projection, a splice mixed with another scroll in one batch
    /// (the historical fail-closed arm), an alt-screen exit mid-batch, the `left_alt`
    /// upper-bound re-check, and a whole-interval eviction at the history floor — and
    /// no `PressCustody` action has that shape: the two undamaged-output actions leave
    /// the selection frame-unchanged, and the two destroying ones are guarded on
    /// damage. Such a batch records
    /// [`CustodyTransition::OutputTookTheSelectionUnattributed`], which is not a model
    /// action and carries a `last_event` outside the model's tag space, so it can
    /// never be validated as if it were one.
    ///
    /// It USED to empty the record instead, and that was the wrong half of a correct
    /// argument. Not MISLABELLING it was right — answering "why did my selection
    /// disappear?" with the name of an innocent event is worse than saying nothing.
    /// But `= None` does not say "not by anything I can name"; it says `last=none`,
    /// which is what a terminal that has never done anything says, and it threw away
    /// whatever true record was standing. These five paths are exactly the flagship
    /// complaint — a highlight gone with no key press to blame — so they are the last
    /// place to destroy evidence. The named variant rules out the keyboard, which is
    /// what the argument wanted in the first place.
    ///
    /// AT LIVE. A batch that damages the selection while the view is at LIVE — a
    /// status line or progress bar repainting the rows a highlight sits on — is the
    /// commonest real instance of `OutputDamagesTheSelectedRows` and is recorded under
    /// that name unconditionally. The model admits it: the action is guarded
    /// `selection == 1 && offset <= MaxOffset - 1` and rides the arriving lines only
    /// when the user owns the viewport (`offset = if owner == 1 { offset + 1 } else
    /// { 0 }`), so both ownerships are in the machine and Tier-1 drives both.
    ///
    /// STILL UNMODELLED, stated rather than papered over: a DAMAGING batch whose
    /// re-pin saturated at the history floor. Its offset does not rise, and the
    /// action's `offset + 1` at `owner == 1` mandates that it does. Unlike the
    /// undamaged twin below it is recorded anyway, because the two cases are not
    /// alike: for undamaged output the only alternative name is `OutputAtLive`, which
    /// would be FALSE (the view is not at live), whereas
    /// `OutputDamagesTheSelectedRows` is a TRUE statement about what happened and only
    /// the offset arithmetic is out of the model's range. A user whose highlight was
    /// just overwritten gets the true answer; the modelling gap is listed in
    /// `press_custody_conformance`'s KNOWN GAPS.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PressCustody",
            action = "OutputAtLive",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PressCustody",
            action = "OutputWhileReading",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PressCustody",
            action = "OutputDamagesTheSelectedRows",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PressCustody",
            action = "OutputInvalidatesTheCoordinateSpace",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    pub(crate) fn note_output_custody(
        &mut self,
        pinned_offset: usize,
        damage: OutputDamage,
        lines_added: u64,
        selection_before: bool,
    ) {
        // The offset site 3 exists to see. `note_output_custody` is called only on a
        // batch that did NOT switch screens, so this is the same grid `pinned_offset`
        // was read from and the pair is a statement about one coordinate space.
        let after = self.grid.display_offset();
        let selection_now = self.text_selection.has_selection();
        let transition = match damage {
            // The VARIANT, not the bool: `clears_selection` answers `true` for `All`
            // without consulting the overlap predicate, so keying off the bool would
            // report a band hit for every ED 3 and this transition would never fire.
            OutputDamage::All => CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            OutputDamage::Hit => CustodyTransition::OutputDamagesTheSelectedRows,
            OutputDamage::None | OutputDamage::Missed => {
                if selection_before && !selection_now {
                    // Destroyed, but not by damage. Its own name, outside the model's
                    // tag space — never `None`, which erases a true prior record and
                    // reads as "this terminal has never done anything".
                    CustodyTransition::OutputTookTheSelectionUnattributed
                } else if lines_added == 0 {
                    // No row entered scrollback: a batch of cursor moves or an OSC
                    // query. No model action describes it either, and it took nothing,
                    // so the previous record still stands.
                    return;
                } else if pinned_offset > 0 {
                    if after <= pinned_offset {
                        // The re-pin SATURATED at the history floor: the arriving line
                        // evicted the oldest, so the same content is no longer
                        // reachable and the offset did not move. `OutputWhileReading`
                        // mandates the rise and `OutputAtLive` would claim the view is
                        // at live, so neither is true. Record nothing — it took
                        // nothing, and the previous record still stands.
                        return;
                    }
                    CustodyTransition::OutputWhileReading
                } else {
                    CustodyTransition::OutputAtLive
                }
            }
        };
        // The EXACT `took`, not the per-variant default: this is the one recorder
        // that can see the offset on both sides of the event and the selection on
        // both sides of the batch. An ED 3 at a live, unselected prompt and a
        // one-line `tail` at live both took nothing, and both say so.
        let took = after != pinned_offset || (selection_before && !selection_now);
        self.note_custody_at(transition, took);
    }
}
