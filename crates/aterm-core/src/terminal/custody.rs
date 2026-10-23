// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Press custody: records which transition last affected the reading position
//! (`display_offset`) or highlight (`TextSelection`).
//!
//! `last_custody` tracks every transition for conformance; `last_custody_change`
//! latches only transitions that moved the offset or cleared a live highlight.

use super::state::Terminal;

/// Which custody transition last fired: one variant per `PressCustody` action.
///
/// The press seam resolves overlapping press bits in the order
/// Release > Repeat > Inert > Typing (`aterm_gui::app_input::PressKind`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CustodyTransition {
    /// The user moved the viewport back into history, raising `display_offset`.
    UserScroll,
    /// The user moved the viewport down toward live without reaching the tail.
    UserScrollTowardLive,
    /// The user brought the viewport back to live without typing.
    SnapToLive,
    /// A user gesture that creates or completes a selection.
    UserSelect,
    /// A user gesture that deliberately clears the selection.
    UserClear,
    /// A byte-producing press that snaps the viewport to live and clears the selection.
    TypingPress,
    /// An auto-repeat tick of a hold whose initial press already took custody (inert).
    RepeatPress,
    /// A bare modifier or lock key press (inert).
    InertPress,
    /// A key release report (inert).
    ReleaseEvent,
    /// Program output arrived while the tail-follower owned the viewport.
    OutputAtLive,
    /// Program output arrived while the user owned the viewport, raising the offset to keep the view pinned.
    OutputWhileReading,
    /// Program output replaced the selected rows, clearing the highlight while keeping the reading position.
    OutputDamagesTheSelectedRows,
    /// [`Self::OutputDamagesTheSelectedRows`] without the view moving under a reader.
    OutputDamagesTheSelectedRowsInPlace,
    /// ED 3 / `clear_scrollback` / RIS destroyed the coordinate space, returning to live and clearing the selection.
    OutputInvalidatesTheCoordinateSpace,
    /// Output cleared a live selection for a reason other than damage overlap while keeping the reading position.
    OutputTookTheSelectionUnattributed,
    /// [`Self::OutputTookTheSelectionUnattributed`] without the view moving under a reader.
    OutputTookTheSelectionUnattributedInPlace,
}

impl CustodyTransition {
    /// Returns the `PressCustody` model action name for this transition.
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

    /// Returns the `PressCustody` model's `last_event` tag (0..=8).
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

    /// Whether [`Terminal::note_custody`] unconditionally latches `last_custody_change` for this transition.
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

    /// Whether this transition is a reader gesture counted by [`Terminal::reader_gesture_count`].
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

    /// Whether this transition destroyed a live highlight without the user releasing it.
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

/// How an output batch's `SelectionDamage` landed on the live selection in `post_process`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum OutputDamage {
    /// No damage was recorded, or recorded bands missed the selection.
    #[default]
    None,
    /// Bands were recorded and none of them intersected the selection.
    Missed,
    /// A recorded band intersected and cleared the selection.
    Hit,
    /// `SelectionDamage::All` destroyed the coordinate space.
    All,
}

impl Terminal {
    /// Records which custody transition just fired and whether it changed the offset or cleared a highlight.
    #[inline]
    pub(crate) const fn note_custody_at(&mut self, transition: CustodyTransition, took: bool) {
        self.last_custody = Some(transition);
        if transition.is_reader_gesture() {
            self.reader_gestures = self.reader_gestures.wrapping_add(1);
        }
        if took {
            self.last_custody_change = Some(transition);
        }
        if transition.takes_a_selection_the_user_did_not_release() {
            self.last_selection_taker = Some(transition);
        }
    }

    /// Calls [`Self::note_custody_at`] using [`CustodyTransition::always_takes_custody`].
    #[inline]
    pub const fn note_custody(&mut self, transition: CustodyTransition) {
        self.note_custody_at(transition, transition.always_takes_custody());
    }

    /// Latches `last_custody_change` without overwriting `last_custody`.
    #[inline]
    pub const fn note_custody_took(&mut self, transition: CustodyTransition) {
        self.last_custody_change = Some(transition);
    }

    /// Total reader gestures ([`CustodyTransition::is_reader_gesture`]) and returns to live recorded.
    #[must_use]
    #[inline]
    pub const fn reader_gesture_count(&self) -> u64 {
        self.reader_gestures
    }

    /// Last recorded custody transition, or `None` if none has fired since creation.
    #[must_use]
    #[inline]
    pub const fn last_custody_transition(&self) -> Option<CustodyTransition> {
        self.last_custody
    }

    /// Last transition that moved the reading position or cleared a highlight (`changed=`).
    #[must_use]
    #[inline]
    pub const fn last_custody_change(&self) -> Option<CustodyTransition> {
        self.last_custody_change
    }

    /// Last transition that cleared a live highlight without the user releasing it.
    #[must_use]
    #[inline]
    pub const fn last_selection_taker(&self) -> Option<CustodyTransition> {
        self.last_selection_taker
    }

    /// Records a user viewport scroll transition from `before` to the current `display_offset`.
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

    /// Takes the last recorded custody transition, leaving `None`.
    #[inline]
    #[cfg(any(test, debug_assertions))]
    pub const fn take_custody_transition(&mut self) -> Option<CustodyTransition> {
        self.last_custody.take()
    }

    /// Records the output custody transition after `process_at` re-pins the viewport.
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
        let after = self.grid.display_offset();
        let selection_now = self.text_selection.has_selection();
        let in_place = pinned_offset > 0 && after <= pinned_offset;
        let transition = match damage {
            OutputDamage::All => CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            OutputDamage::Hit if in_place => CustodyTransition::OutputDamagesTheSelectedRowsInPlace,
            OutputDamage::Hit => CustodyTransition::OutputDamagesTheSelectedRows,
            OutputDamage::None | OutputDamage::Missed => {
                if selection_before && !selection_now {
                    // Destroyed, but not by damage: record it rather than erase a
                    if in_place {
                        CustodyTransition::OutputTookTheSelectionUnattributedInPlace
                    } else {
                        CustodyTransition::OutputTookTheSelectionUnattributed
                    }
                } else if lines_added == 0 {
                    return;
                } else if pinned_offset > 0 {
                    if after <= pinned_offset {
                        return;
                    }
                    CustodyTransition::OutputWhileReading
                } else {
                    CustodyTransition::OutputAtLive
                }
            }
        };
        let took = after != pinned_offset || (selection_before && !selection_now);
        self.note_custody_at(transition, took);
    }
}
