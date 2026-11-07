// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One prepared restart, its positively established terminal fact, a pending
/// input drain, and repeated publication through the authenticated journal.
/// Process quiescence and grant admission are premises checked by other models;
/// this composition checks that the publication boundary preserves those facts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn terminal_publication_model() -> Model {
    aterm_spec::ty_model! {
        TerminalPublication {
            const Buggy = 0;
            // kind: open, NoExec, Unknown. journal: prepared, may-execute,
            // NoExec, sealed without possible execution, sealed with it.
            var kind = 0; var may = 0; var drained = 0;
            var journal = 0; var captured = 0; var reply = 0;
            var published = 0; var observed = 0;
            // 0 forward, 1 rollback, 2 no phase-bearing receipt/observation.
            var active = 0; var journal_phase = 2; var observed_phase = 2;
            action StartRollback when (kind == 0 && active == 0) { active = 1; }
            action MayExecute when (kind == 0 && may == 0) {
                may = 1; journal = 1;
            }
            action CloseNoExec when (kind == 0 && may == 0) { kind = 1; }
            action CloseUnknown when (kind == 0) { kind = 2; }
            action Drain when (kind > 0 && drained == 0) { drained = 1; }
            action Capture when (kind > 0 && captured == 0) {
                reply = if kind == 1 && drained == 1 {1} else {0};
                captured = 1;
            }
            // A different request (e.g. precommit) can return Unknown even when
            // the underlying operation already has a settled NoExec fact.
            action CaptureUnknown when (kind > 0 && captured == 0) {
                reply = 0; captured = 1;
            }
            action PublishAck when (captured == 1) {
                journal = if drained == 1 || Buggy == 3 {
                    if kind == 1 {if reply == 0 && Buggy == 1 {3} else {2}} else {if Buggy == 5 {2} else {if may == 1 {4} else {3}}}
                } else {journal};
                // Updates read the pre-state, so select the phase from the
                // publication conditions rather than the newly written journal.
                journal_phase = if drained == 1 || Buggy == 3 {
                    if kind == 1 {if reply == 0 && Buggy == 1 {journal_phase} else {if Buggy == 6 {0} else {active}}}
                    else {if Buggy == 5 {active} else {journal_phase}}
                } else {journal_phase};
                published = if reply == 1 && drained == 1 {1} else {published};
                captured = 0;
            }
            // The append committed, but its acknowledgement did not arrive.
            // The caller withholds success; an exact retry is still possible.
            action PublishLostAck when (captured == 1) {
                journal = if drained == 1 {
                    if kind == 1 {if reply == 0 && Buggy == 1 {3} else {2}} else {if Buggy == 5 {2} else {if may == 1 {4} else {3}}}
                } else {journal};
                journal_phase = if drained == 1 {
                    if kind == 1 {if reply == 0 && Buggy == 1 {journal_phase} else {if Buggy == 6 {0} else {active}}}
                    else {if Buggy == 5 {active} else {journal_phase}}
                } else {journal_phase};
                captured = 0;
            }
            action PublishFailed when (captured == 1) {
                published = if reply == 1 && Buggy == 2 {1} else {published};
                captured = 0;
            }
            action Observe {
                observed = if journal == 2 || Buggy == 4 {1} else {observed};
                observed_phase = if journal == 2 {if Buggy == 7 {1 - journal_phase} else {journal_phase}} else {observed_phase};
            }
            invariant NoExecRemainsRecoverable: kind == 0 || kind == 2 || journal == 0 || journal == 2;
            invariant PublishedOnlyAfterCommit: published == 0 || journal == 2;
            invariant TerminalWriteAfterDrain: journal <= 1 || drained == 1;
            invariant ObservationNeedsPositiveEvidence: observed == 0 || (journal == 2 && kind == 1 && may == 0 && drained == 1);
            invariant PossibleExecutionStaysOpaque: may == 0 || journal == 1 || journal == 4;
            invariant JournalCarriesClosedPhase: journal <= 1 || journal > 2 || journal_phase == active;
            invariant ObservationCarriesClosedPhase: observed == 0 || observed_phase == active;
            bound Finite: 0 <= kind && kind <= 2 && 0 <= may && may <= 1
                && 0 <= drained && drained <= 1 && 0 <= journal && journal <= 4
                && 0 <= captured && captured <= 1 && 0 <= reply && reply <= 1
                && 0 <= published && published <= 1 && 0 <= observed && observed <= 1
                && 0 <= active && active <= 1 && 0 <= journal_phase && journal_phase <= 2
                && 0 <= observed_phase && observed_phase <= 2;
        }
    }
}
