// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE HARNESS JUST DID, AND HOW A LANDING ENDED, as the window shows
//! them (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9): the words both
//! ends of the wire share.
//!
//! * [`Act`] — one act of the harness's that the Autopilot chip may light for
//!   a moment (§9.2.1's seven), with its glyph, its word and its words for a
//!   screen reader. Told by the loop's single ledger writer through
//!   `IdleHost::act_taken` ([`crate::supervise::IdleHost::act_taken`]).
//! * [`Counter`] — the counters of Settings ▸ Harness's WHAT THE HARNESS DID
//!   ([`super::stats::HarnessStats`]) a ledger row adds to. An act and its
//!   counter differ where the stats table already makes them differ: a
//!   question dialog's key lights `Answered` and counts nothing (the
//!   journal's `chose` counts the dialog, once).
//! * [`TaskFace`] — the task's face under Land (§9.4.2): open, landed, done,
//!   spent or theirs, and the word `status task=` prints for it. Told by the
//!   loop through `IdleHost::task_face`
//!   ([`crate::supervise::IdleHost::task_face`]).
//!
//! Words only: nothing here reads a ledger, a screen or a clock. The one sort
//! of a ledger row into its act and its counter is the act ticker's
//! (§9.2.3); until it lands no loop tells an act, and no window lights one.

/// One act of the harness's, as the Autopilot chip names it for a moment
/// (§9.2.1): the seven the page's own act families hold (approved, declined,
/// answered, continued, resumed, landed — Land's instruction and its nudge
/// apart). Nothing else lights the chip: not a `skipped`, `deferred`,
/// `waiting` or `escalated` row, and not the `typed` rows the page counts
/// only by rule (`/compact`, `/model`, a login, a consent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// A box approved (`approved` under a box's rule).
    Approved,
    /// A box declined with a reason (`declined`).
    Declined,
    /// A question answered: a dialog's keys (one label for the whole dialog)
    /// or a prose answer typed (`answer@v1`, `prose-*@v1`, `land-answer@v1`).
    Answered,
    /// The work kept moving (`continue@v1`, `continue-suggestion@v1`,
    /// `continue-done-check@v1`, `continue-open-items@v1`).
    Continued,
    /// Carried past a wall: a usage reset, an API error, a cut reply, a full
    /// context's continuation (not its `/compact`).
    Resumed,
    /// Land's instruction typed (`land@v1`).
    Landing,
    /// A land nudge typed (`land-nudge@v1`).
    Nudged,
}

impl Act {
    /// Every act, in §9.2.1's order.
    pub const ALL: [Act; 7] = [
        Act::Approved,
        Act::Declined,
        Act::Answered,
        Act::Continued,
        Act::Resumed,
        Act::Landing,
        Act::Nudged,
    ];

    /// The act's glyph, drawn before its word: `✓`, `✗`, `?`, `↻`, `⧗` (the
    /// footer's own wall mark) and `⤓` for both of Land's.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Act::Approved => "\u{2713}",
            Act::Declined => "\u{2717}",
            Act::Answered => "?",
            Act::Continued => "\u{21bb}",
            Act::Resumed => "\u{29d7}",
            Act::Landing | Act::Nudged => "\u{2913}",
        }
    }

    /// The act's word, drawn after its glyph and a space (`✓ approved`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Act::Approved => "approved",
            Act::Declined => "declined",
            Act::Answered => "answered",
            Act::Continued => "kept going",
            Act::Resumed => "resumed",
            Act::Landing => "landing",
            Act::Nudged => "nudged",
        }
    }

    /// The act in words a screen reader says, no glyph: letters and spaces
    /// only, in §9.2.1's form (`Last: approved at 14:05.`) — the label's own
    /// word, but for Land's instruction, whose `landing` would read as a
    /// landing still to come at that time: `started landing`.
    #[must_use]
    pub const fn spoken(self) -> &'static str {
        match self {
            Act::Landing => "started landing",
            act => act.word(),
        }
    }
}

/// A counter of WHAT THE HARNESS DID ([`super::stats::HarnessStats`]) one
/// ledger row adds to; a row with none counts only by its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Counter {
    /// [`super::stats::HarnessStats::approved`].
    Approved,
    /// [`super::stats::HarnessStats::declined`].
    Declined,
    /// [`super::stats::HarnessStats::answered`].
    Answered,
    /// [`super::stats::HarnessStats::continued`].
    Continued,
    /// [`super::stats::HarnessStats::resumed`].
    Resumed,
    /// [`super::stats::HarnessStats::landed`]: Land's instruction and its
    /// nudges.
    Landed,
}

/// The task's face under Land (§9.4.2), told by the loop as it moves.
///
/// | Face | Means | `task=` |
/// |---|---|---|
/// | [`TaskFace::Open`] | the work goes on, or a landing is under way | `open` |
/// | [`TaskFace::Landed`] | the agent finished after the land instruction | `landed` |
/// | [`TaskFace::Done`] | the agent said it was finished before a landing was asked | `done` |
/// | [`TaskFace::Spent`] | no done reply after the instruction and its nudges | `spent` |
/// | [`TaskFace::Theirs`] | a person's own exchange | `theirs` |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskFace {
    /// Nothing ended: the work goes on, or a landing is under way.
    Open,
    /// A done reply to a land act: the harness types nothing more until
    /// someone else's turn.
    Landed,
    /// The done check or `FINISHED` ended it, not Land: nothing more is
    /// typed, but nobody asked it to verify, commit and push.
    Done,
    /// The landing is spent: the instruction and its nudges typed, no done
    /// reply. Nothing more is typed but a land answer to a question.
    Spent,
    /// A person's own exchange: the harness types nothing until someone
    /// else's turn ends.
    Theirs,
}

impl TaskFace {
    /// Every face, in §9.4.1's order.
    pub const ALL: [TaskFace; 5] = [
        TaskFace::Open,
        TaskFace::Landed,
        TaskFace::Done,
        TaskFace::Spent,
        TaskFace::Theirs,
    ];

    /// The word `status` and `ls` print for it (`task=`).
    #[must_use]
    pub const fn status(self) -> &'static str {
        match self {
            TaskFace::Open => "open",
            TaskFace::Landed => "landed",
            TaskFace::Done => "done",
            TaskFace::Spent => "spent",
            TaskFace::Theirs => "theirs",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each act's glyph and word are its own pair (Land's two share the
    /// glyph and differ in the word), and its spoken words carry no glyph:
    /// the label's word, as §9.2.1 says it (`approved`), but Land's
    /// instruction (`started landing`), and no two acts say the same.
    #[test]
    fn every_act_has_its_face_and_words_with_no_glyph() {
        let mut faces = std::collections::BTreeSet::new();
        let mut said = std::collections::BTreeSet::new();
        assert_eq!(Act::Approved.spoken(), "approved");
        assert_eq!(Act::Landing.spoken(), "started landing");
        for act in Act::ALL {
            assert!(faces.insert((act.glyph(), act.word())), "{act:?}");
            assert!(said.insert(act.spoken()), "{act:?}: {}", act.spoken());
            if act != Act::Landing {
                assert_eq!(act.spoken(), act.word(), "{act:?}");
            }
            assert!(
                act.spoken()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == ' '),
                "{act:?}: {}",
                act.spoken()
            );
        }
    }

    /// The five faces print five words.
    #[test]
    fn every_face_prints_its_own_status_word() {
        let words: std::collections::BTreeSet<_> =
            TaskFace::ALL.iter().map(|f| f.status()).collect();
        assert_eq!(
            words.into_iter().collect::<Vec<_>>(),
            ["done", "landed", "open", "spent", "theirs"]
        );
    }
}
