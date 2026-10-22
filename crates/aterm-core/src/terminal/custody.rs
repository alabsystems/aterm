// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Press custody: the record of which custody transition last fired.
//!
//! Custody covers the two things the user owns: the reading position (the
//! grid's `display_offset`) and the highlight (`TextSelection`). Many events can
//! move either, and the record names which one did, so the `custody` control
//! verb can answer "why did my selection disappear?". Several events (an
//! auto-repeat tick, a bare modifier, a key release) leave identical state, so
//! the record carries the discriminator rather than inferring it from the state
//! change; conformance can then check that "a repeat is inert".
//!
//! Cost: `Option<CustodyTransition>` fields on [`Terminal`], one byte each in
//! existing padding. Recording is a store and a compare at sites that already
//! hold the terminal mutably; the key-release arm of the input seam and the
//! hidden-session repeat path take the lock for it and say so at the call site.
//!
//! `last_custody` is the last event, which conformance needs. The most frequent
//! writer is [`CustodyTransition::OutputAtLive`] (every prompt, `cat`, `tail -f`),
//! so `last_custody_change` latches only transitions that took something (the
//! offset moved or a live highlight died). The `custody` verb prints both
//! (`last=` and `changed=`).
//!
//! A record is not a self-reported "this disturbed something" flag.
//! `PressCustody`'s invariants are stated over the observable state (offset,
//! owner, selection) against the pre-action values; the name only selects the
//! action to check. A repeat that moved the viewport still fails
//! `RepeatPressIsInert` whatever it records.

use super::state::Terminal;

/// Which custody transition last fired: one variant per `PressCustody` action.
///
/// The three inert press classes (release, auto-repeat, bare modifier) are
/// separate variants although state-identical, so a regression in one is
/// visible.
///
/// The press bits overlap (`is_modifier_or_lock_key` looks only at the key, so
/// a Shift key-up is both release and inert modifier), and the press seam
/// resolves them in the fixed order Release > Repeat > Inert > Typing (see
/// `aterm_gui::app_input::PressKind`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CustodyTransition {
    /// The user moved the viewport back into history (wheel, PgUp, the `scroll`
    /// verb, a selection drag past the grid edge). Only a move that raises the
    /// offset; a move down is [`Self::UserScrollTowardLive`] or
    /// [`Self::SnapToLive`].
    UserScroll,
    /// The user moved the viewport back down toward live without reaching it (a
    /// wheel notch or PgDn that stops short of the tail). The highlight stays.
    UserScrollTowardLive,
    /// The user brought the viewport back to live without typing: End,
    /// `ScrollToBottom`, a downward scroll landing at the tail, the paste and IME
    /// snaps, the find bar's restore. Ownership goes to the tail-follower; the
    /// highlight stays.
    SnapToLive,
    /// A user gesture that leaves a selection behind — the press that starts a drag,
    /// and the release that completes one.
    UserSelect,
    /// A user gesture that deliberately deselects: a press and release inside one
    /// cell. Always allowed; it is the user's own highlight.
    UserClear,
    /// The one handover: a byte-producing press means "take me back to the prompt",
    /// so the viewport snaps to live and the selection goes.
    TypingPress,
    /// An auto-repeat tick of a hold whose press already took custody. Inert:
    /// re-snapping at the repeat rate would destroy scrolls and selections made
    /// mid-hold.
    RepeatPress,
    /// A bare modifier or lock key (Shift/Control/Alt/Super/Caps…). Inert: it is
    /// the first half of Cmd-C, which must not clear the selection.
    InertPress,
    /// A key release report (Kitty `REPORT_EVENT_TYPES`). Inert — a key-up is not
    /// typing.
    ReleaseEvent,
    /// Program output arrived while the tail-follower owned the viewport. The view
    /// stays at live because it already was; the content advances underneath it.
    OutputAtLive,
    /// Program output arrived while the user owned the viewport. The re-pin rides
    /// the arriving lines so the same content stays in view: the offset rises
    /// and ownership stays with the user.
    OutputWhileReading,
    /// Program output replaced the rows the selection sat on. The highlight dies
    /// (a copy would return text the user never selected), but the reading
    /// position stays: the re-pin rides the arriving lines.
    OutputDamagesTheSelectedRows,
    /// [`Self::OutputDamagesTheSelectedRows`] without the view moving under a
    /// reader: no rows advanced (an in-place rewrite such as `\r` plus EL, a
    /// DECERA, a repainting status line) or the re-pin saturated at the history
    /// floor.
    OutputDamagesTheSelectedRowsInPlace,
    /// ED 3 / `clear_scrollback` / RIS: the coordinate space of the offset and
    /// the selection anchors is gone, so the viewport returns to the
    /// tail-follower and the selection dies.
    OutputInvalidatesTheCoordinateSpace,
    /// Output took a live highlight for a reason other than damage overlap: a
    /// malformed or saturated splice projection, a splice mixed with another
    /// scroll, an alt-screen exit mid-batch, the `left_alt` upper-bound re-check,
    /// or a whole-interval eviction at the history floor. Same `PressCustody` tag
    /// (6) and law as the damaging take: the highlight may go, the reading
    /// position may not. Recording it (instead of nothing) tells the user that
    /// output, not the keyboard, took the selection.
    OutputTookTheSelectionUnattributed,
    /// [`Self::OutputTookTheSelectionUnattributed`] without the view moving under
    /// a reader (no row arrived, or the re-pin saturated).
    OutputTookTheSelectionUnattributedInPlace,
}

impl CustodyTransition {
    /// The `PressCustody` action this transition is, by the model's spelling.
    /// Conformance takes the action name from here, the engine's own
    /// classification.
    #[inline]
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::UserScroll => "UserScroll",
            Self::UserScrollTowardLive => "UserScrollTowardLive",
            Self::SnapToLive => "SnapToLive",
            Self::UserSelect => "UserSelect",
            Self::UserClear => "UserClear",
            Self::TypingPress => "TypingPress",
            Self::RepeatPress => "RepeatPress",
            Self::InertPress => "InertPress",
            Self::ReleaseEvent => "ReleaseEvent",
            Self::OutputAtLive => "OutputAtLive",
            Self::OutputWhileReading => "OutputWhileReading",
            Self::OutputDamagesTheSelectedRows => "OutputDamagesTheSelectedRows",
            Self::OutputDamagesTheSelectedRowsInPlace => "OutputDamagesTheSelectedRowsInPlace",
            Self::OutputInvalidatesTheCoordinateSpace => "OutputInvalidatesTheCoordinateSpace",
            Self::OutputTookTheSelectionUnattributed => "OutputTookTheSelectionUnattributed",
            Self::OutputTookTheSelectionUnattributedInPlace => {
                "OutputTookTheSelectionUnattributedInPlace"
            }
        }
    }

    /// The model's `last_event` tag: 0 a user gesture, 1 typing, 2 auto-repeat,
    /// 3 a bare modifier, 4 a release, 5 output that missed the selected rows,
    /// 6 output that took the highlight, 7 output that invalidated the
    /// coordinate space, 8 a non-typing gesture back toward live.
    ///
    /// Not injective, as in the model: the variant is the discriminator, the tag
    /// its projection.
    #[inline]
    #[must_use]
    pub const fn last_event(self) -> i64 {
        match self {
            Self::UserScroll | Self::UserSelect | Self::UserClear => 0,
            Self::TypingPress => 1,
            Self::RepeatPress => 2,
            Self::InertPress => 3,
            Self::ReleaseEvent => 4,
            Self::OutputAtLive | Self::OutputWhileReading => 5,
            Self::OutputDamagesTheSelectedRows
            | Self::OutputDamagesTheSelectedRowsInPlace
            | Self::OutputTookTheSelectionUnattributed
            | Self::OutputTookTheSelectionUnattributedInPlace => 6,
            Self::OutputInvalidatesTheCoordinateSpace => 7,
            Self::UserScrollTowardLive | Self::SnapToLive => 8,
        }
    }

    /// Whether this transition, at every site that records it, has already
    /// taken something from the user: the latch condition for
    /// `last_custody_change`.
    ///
    /// True by the recording condition: [`Self::UserScroll`] is recorded only on
    /// a rise and the downward gestures only on a fall,
    /// [`Self::OutputWhileReading`] only when the re-pin moved the offset, and
    /// the destroying output kinds only when a highlight died. The press classes
    /// are false (three are inert; [`Self::TypingPress`] may take nothing, and
    /// `apply_press_custody` promotes it explicitly when it does).
    /// [`Self::OutputAtLive`] is an identity transition.
    ///
    /// The output rows are defaults only: [`Terminal::note_output_custody`]
    /// computes the exact answer ([`Terminal::note_custody_at`]).
    ///
    /// [`Self::UserClear`] latches even for a deselecting click with nothing
    /// selected; it is still a gesture expressing intent about the highlight.
    #[inline]
    #[must_use]
    pub const fn always_takes_custody(self) -> bool {
        match self {
            Self::UserScroll
            | Self::UserScrollTowardLive
            | Self::SnapToLive
            | Self::UserSelect
            | Self::UserClear
            | Self::OutputWhileReading
            | Self::OutputDamagesTheSelectedRows
            | Self::OutputDamagesTheSelectedRowsInPlace
            | Self::OutputInvalidatesTheCoordinateSpace
            | Self::OutputTookTheSelectionUnattributed
            | Self::OutputTookTheSelectionUnattributedInPlace => true,
            Self::TypingPress
            | Self::RepeatPress
            | Self::InertPress
            | Self::ReleaseEvent
            | Self::OutputAtLive => false,
        }
    }

    /// Whether this is a gesture of the person reading the session (typing,
    /// paste, scroll, selection, return to live) rather than output, a bare
    /// modifier or a key release. Counted by [`Terminal::reader_gesture_count`].
    #[inline]
    #[must_use]
    pub const fn is_reader_gesture(self) -> bool {
        match self {
            Self::UserScroll
            | Self::UserScrollTowardLive
            | Self::SnapToLive
            | Self::UserSelect
            | Self::UserClear
            | Self::TypingPress
            | Self::RepeatPress => true,
            Self::InertPress
            | Self::ReleaseEvent
            | Self::OutputAtLive
            | Self::OutputWhileReading
            | Self::OutputDamagesTheSelectedRows
            | Self::OutputDamagesTheSelectedRowsInPlace
            | Self::OutputInvalidatesTheCoordinateSpace
            | Self::OutputTookTheSelectionUnattributed
            | Self::OutputTookTheSelectionUnattributedInPlace => false,
        }
    }

    /// Whether this transition destroyed a live highlight without the user
    /// asking: output that replaced the selected rows, output that destroyed the
    /// coordinate space, output that took it for an unattributed reason, and
    /// typing (a documented but surprising side effect).
    #[inline]
    #[must_use]
    pub const fn takes_a_selection_the_user_did_not_release(self) -> bool {
        matches!(
            self,
            Self::OutputDamagesTheSelectedRows
                | Self::OutputDamagesTheSelectedRowsInPlace
                | Self::OutputInvalidatesTheCoordinateSpace
                | Self::OutputTookTheSelectionUnattributed
                | Self::OutputTookTheSelectionUnattributedInPlace
                | Self::TypingPress
        )
    }
}

/// How this output batch's `SelectionDamage` landed on the live selection, as
/// classified in `post_process`.
///
/// `SelectionDamage::clears_selection` short-circuits `All` to `true`, so the
/// bool alone cannot tell a band hit from an ED 3 / RIS, and the invalidation
/// transition would never be recorded.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum OutputDamage {
    /// No damage was recorded, or recorded bands missed the selection; both
    /// leave the highlight alone. `Missed` distinguishes the second for
    /// diagnostics.
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
    /// Record which custody transition just fired, and whether it took
    /// something.
    ///
    /// Called from the site that made the decision, while it still has the
    /// facts that separate the variants (several leave identical state). `took`
    /// is that site's observation that the offset changed or a live highlight
    /// died; only it has both sides of the change.
    #[inline]
    pub(crate) fn note_custody_at(&mut self, transition: CustodyTransition, took: bool) {
        self.last_custody = Some(transition);
        if transition.is_reader_gesture() {
            self.reader_gestures = self.reader_gestures.wrapping_add(1);
        }
        if took {
            self.last_custody_change = Some(transition);
        }
        // Also latch involuntary takers separately: `last_custody_change` is
        // overwritten by an ordinary click (`UserClear`), which would bury the
        // explanation of a lost highlight.
        if transition.takes_a_selection_the_user_did_not_release() {
            self.last_selection_taker = Some(transition);
        }
    }

    /// [`Self::note_custody_at`] for the sites whose recording condition already
    /// decides the answer — see [`CustodyTransition::always_takes_custody`].
    #[inline]
    pub fn note_custody(&mut self, transition: CustodyTransition) {
        self.note_custody_at(transition, transition.always_takes_custody());
    }

    /// Latch `last_custody_change` only, leaving the last-event record alone. For
    /// `apply_press_custody`, which stamps the press class before acting and only
    /// then learns whether it took anything; a promotion is not a second event,
    /// and a press class that started disturbing still fails as the transition
    /// it is.
    #[inline]
    pub fn note_custody_took(&mut self, transition: CustodyTransition) {
        self.last_custody_change = Some(transition);
    }

    /// How many reader gestures ([`CustodyTransition::is_reader_gesture`]) this
    /// session has recorded, plus returns to live that found the view already
    /// there ([`Self::return_to_live`]). Only changes are meaningful: a later
    /// sample that differs means the person acted on the session in between.
    #[must_use]
    #[inline]
    pub const fn reader_gesture_count(&self) -> u64 {
        self.reader_gestures
    }

    /// The last recorded custody transition, or `None` if nothing has moved
    /// custody since creation. Non-consuming (the `custody` verb's read).
    #[must_use]
    #[inline]
    pub const fn last_custody_transition(&self) -> Option<CustodyTransition> {
        self.last_custody
    }

    /// The last recorded transition that took the reading position or the
    /// highlight: the `changed=` half of the `custody` verb. Survives the
    /// ordinary output that overwrites [`Self::last_custody_transition`].
    #[must_use]
    #[inline]
    pub const fn last_custody_change(&self) -> Option<CustodyTransition> {
        self.last_custody_change
    }

    /// The last transition that took a live highlight the user did not release.
    /// Survives later activity so the explanation is not erased. Non-consuming.
    #[must_use]
    #[inline]
    pub const fn last_selection_taker(&self) -> Option<CustodyTransition> {
        self.last_selection_taker
    }

    /// The viewport half of the record: the user moved the reading position.
    ///
    /// `before` is `display_offset` sampled just before the move, inside the same
    /// borrow. A rise is [`CustodyTransition::UserScroll`]. A fall is a
    /// non-typing gesture toward live (scroll down, wheel down, selection
    /// autoscroll down, a search jump below the view): [`CustodyTransition::SnapToLive`]
    /// when it lands at the tail, else [`CustodyTransition::UserScrollTowardLive`].
    /// No move, no record.
    ///
    /// Recorded in the `Terminal` primitives that can raise the offset
    /// ([`Terminal::scroll_display`], [`Terminal::scroll_to_absolute_row`],
    /// [`Terminal::scroll_to_top`]), so every input path is covered. Only the raw
    /// `Terminal::grid_mut().scroll_display(..)` bypasses it, used by test
    /// fixtures and bench support, not by shipping input paths.
    ///
    /// The model moves the offset by one while real gestures move rows or
    /// pages; the record says the user took the viewport, not how far.
    /// Conformance drives one-row forms, and a real wheel notch for the record.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "UserScroll",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "UserScrollTowardLive",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "SnapToLive",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[inline]
    pub(crate) fn note_scroll_custody(&mut self, before: usize) {
        let after = self.grid.display_offset();
        if after > before {
            self.note_custody(CustodyTransition::UserScroll);
        } else if after < before {
            self.note_custody(if after == 0 {
                CustodyTransition::SnapToLive
            } else {
                CustodyTransition::UserScrollTowardLive
            });
        }
    }

    /// Take the last recorded custody transition, leaving `None`. Conformance
    /// clears before a step and requires `Some(_)` after it, proving the step's
    /// site recorded rather than reading a stale tag.
    #[inline]
    #[cfg(any(test, debug_assertions))]
    pub fn take_custody_transition(&mut self) -> Option<CustodyTransition> {
        self.last_custody.take()
    }

    /// The output half of the record: the emit point of `process_at`'s
    /// three-site protocol, where the pre-batch reading position, the damage
    /// verdict and the final post-re-pin offset are all known.
    ///
    /// 1. The prologue latches `pinned_offset`, the pre-batch `display_offset`,
    ///    before forcing it to 0; only it separates
    ///    [`CustodyTransition::OutputAtLive`] from
    ///    [`CustodyTransition::OutputWhileReading`].
    /// 2. `post_process` classifies the damage into an [`OutputDamage`]; it
    ///    cannot emit, since the re-pin has not run.
    /// 3. This, after the re-pin, reads the final offset. A re-pin can saturate
    ///    (a reader at the top of a full scrollback: the new row evicts the
    ///    oldest and the offset does not rise), and `OutputWhileReading` requires
    ///    the rise.
    ///
    /// `lines_added` separates output from nothing: a batch of cursor moves or an
    /// OSC query advances no rows and records nothing. Damage is exempt (ED 3
    /// advances no rows).
    ///
    /// `selection_before` catches batches that destroyed a selection for a
    /// reason other than damage overlap (see
    /// [`CustodyTransition::OutputTookTheSelectionUnattributed`]); they record
    /// that variant or its in-place twin rather than clearing the record.
    ///
    /// Damage at live (a status line repainting under a highlight) records
    /// `OutputDamagesTheSelectedRows`; the model's action rides the arriving
    /// lines only when the user owns the viewport, so both ownerships are
    /// covered.
    ///
    /// A taking batch under a reader whose view did not rise (no row arrived, or
    /// the re-pin saturated) records the `…InPlace` twin, which leaves the offset
    /// where it was. An undamaged batch that did not rise records nothing.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputAtLive",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputWhileReading",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputDamagesTheSelectedRows",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputDamagesTheSelectedRowsInPlace",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputInvalidatesTheCoordinateSpace",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputTookTheSelectionUnattributed",
            project = "aterm_gui::press_custody_conformance::project_press_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PressCustody",
            action = "OutputTookTheSelectionUnattributedInPlace",
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
        // Called only on a batch that did not switch screens, so this is the grid
        // `pinned_offset` was read from.
        let after = self.grid.display_offset();
        let selection_now = self.text_selection.has_selection();
        // A reader whose view did not rise: the taking kinds record their in-place
        // twin, whose model action leaves the offset where it was.
        let in_place = pinned_offset > 0 && after <= pinned_offset;
        let transition = match damage {
            // Key off the variant: `clears_selection` answers `true` for `All`
            // without the overlap test.
            OutputDamage::All => CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            OutputDamage::Hit if in_place => CustodyTransition::OutputDamagesTheSelectedRowsInPlace,
            OutputDamage::Hit => CustodyTransition::OutputDamagesTheSelectedRows,
            OutputDamage::None | OutputDamage::Missed => {
                if selection_before && !selection_now {
                    // Destroyed, but not by damage: record it rather than erase a
                    // true prior record.
                    if in_place {
                        CustodyTransition::OutputTookTheSelectionUnattributedInPlace
                    } else {
                        CustodyTransition::OutputTookTheSelectionUnattributed
                    }
                } else if lines_added == 0 {
                    // No row entered scrollback (cursor moves, an OSC query): no
                    // model action, nothing taken, the previous record stands.
                    return;
                } else if pinned_offset > 0 {
                    if after <= pinned_offset {
                        // The re-pin saturated at the history floor: the offset did
                        // not move, so neither `OutputWhileReading` (needs a rise)
                        // nor `OutputAtLive` (claims live) is true. Record nothing.
                        return;
                    }
                    CustodyTransition::OutputWhileReading
                } else {
                    CustodyTransition::OutputAtLive
                }
            }
        };
        // The exact `took`: this recorder sees the offset and the selection on
        // both sides of the batch, so an ED 3 at a live, unselected prompt
        // correctly reports nothing taken.
        let took = after != pinned_offset || (selection_before && !selection_now);
        self.note_custody_at(transition, took);
    }
}
