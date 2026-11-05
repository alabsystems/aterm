// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Retained execution classification and the single successor slot, before account admission.
/// Kernel facts are monotone only within this captured operation; this model does
/// not claim that a later missing PID or a host restart reconstructs a lost watch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn closed_restart_model() -> Model {
    aterm_spec::ty_model! {
        ClosedRestartEvidence {
            const Buggy = 0;
            var watch = 1; var dead = 0; var group = 0; var settled = 0;
            var granted = 0; var closed = 0; var winner = 0; var claims = 0;
            var retired = 0; var at_retire = 0; var retried = 0;
            var kind = 0; var first_winner = 0;
            var child_dead = 0; var child_group = 0;
            action LoseWatch when (closed == 0 && watch == 1) { watch = 0; }
            action SourceDies when (closed == 0 && dead == 0) { dead = 1; }
            action GroupDrains when (closed == 0 && group == 0) { group = 1; }
            action InputSettles when (closed == 0 && settled == 0) { settled = 1; }
            action IssueExec when (closed == 0 && granted == 0) { granted = 1; }
            action ChildDies when (closed == 0 && granted == 1 && child_dead == 0) { child_dead = 1; }
            action ChildGroupDrains when (closed == 0 && granted == 1 && child_group == 0) { child_group = 1; }
            action Capture when (closed == 0) {
                kind = 1;
                closed = if (watch == 1 || Buggy == 1) && (dead == 1 || Buggy == 2)
                    && (group == 1 || Buggy == 3) && (settled == 1 || Buggy == 4)
                    && (granted == 0 || Buggy == 5) {1} else {0};
            }
            action CaptureExited when (closed == 0) {
                kind = 2;
                closed = if watch == 1 && dead == 1 && group == 1 && settled == 1
                    && (granted == 1 || Buggy == 9)
                    && (granted == 0 || child_dead == 1 || Buggy == 10)
                    && (granted == 0 || child_group == 1 || Buggy == 11) {1} else {0};
            }
            action ClaimA when (closed == 1 && winner == 0) {
                winner = if retired == 0 || Buggy == 7 {1} else {0};
                claims = if retired == 0 || Buggy == 7 {1} else {0};
                first_winner = if retired == 0 || Buggy == 7 {1} else {first_winner};
            }
            action ClaimB when (closed == 1 && winner == 0) {
                winner = if retired == 0 || Buggy == 7 {2} else {0};
                claims = if retired == 0 || Buggy == 7 {1} else {0};
                first_winner = if retired == 0 || Buggy == 7 {2} else {first_winner};
            }
            action Retry when (closed == 1 && winner > 0 && retried == 0) {
                retried = 1; claims = if Buggy == 6 {2} else {claims};
                winner = if Buggy == 8 {if winner == 1 {2} else {1}} else {winner};
            }
            action Retire when (closed == 1 && retired == 0) { retired = 1; at_retire = winner; }
            invariant RetainedWatch: closed == 0 || watch == 1;
            invariant WitnessedDeath: closed == 0 || dead == 1;
            invariant DrainedGroup: closed == 0 || group == 1;
            invariant SettledInput: closed == 0 || settled == 1;
            invariant NeverIssued: closed == 0 || kind == 0 || kind == 2 || granted == 0;
            invariant RecordedIssued: closed == 0 || kind == 0 || kind == 1 || granted == 1;
            invariant ChildDeath: closed == 0 || kind == 0 || kind == 1 || granted == 0 || child_dead == 1;
            invariant ChildGroup: closed == 0 || kind == 0 || kind == 1 || granted == 0 || child_group == 1;
            invariant OneSuccessor: claims <= 1;
            invariant WinnerStable: claims == 0 || winner == first_winner;
            invariant RetiredCannotChangeWinner: retired == 0 || winner == at_retire;
            bound Finite: 0 <= watch && watch <= 1 && 0 <= dead && dead <= 1
                && 0 <= group && group <= 1 && 0 <= settled && settled <= 1
                && 0 <= granted && granted <= 1 && 0 <= closed && closed <= 1
                && 0 <= winner && winner <= 2 && 0 <= claims && claims <= 2
                && 0 <= retired && retired <= 1 && 0 <= at_retire && at_retire <= 2
                && 0 <= retried && retried <= 1 && 0 <= kind && kind <= 2
                && 0 <= first_winner && first_winner <= 2
                && 0 <= child_dead && child_dead <= 1 && 0 <= child_group && child_group <= 1;
        }
    }
}
