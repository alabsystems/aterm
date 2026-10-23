// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models for session process-name resolution and Claude footer model tracking.

use aterm_spec::derive::*;

/// A session owns one latest job and at most one channel token.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn program_resolver_queue_model() -> Model {
    aterm_spec::ty_model! {
        ProgramResolverQueue {
            const Buggy = 0;
            var requests = 0;
            var latest = 0;
            var queued = 0;
            var inflight = 0;
            var snapshot = 0;
            var completed = 0;
            var worker = 1;
            var active = 0;
            var owned = 0;

            action AskFirst when (requests == 0) {
                requests = 1;
                latest = 1;
                queued = 1;
                active = 1;
                owned = 1;
            }
            action Take when (worker == 1 && queued == 1 && inflight == 0 && active == 1) {
                queued = 0;
                inflight = 1;
                snapshot = latest;
            }
            action DropCancelled when (worker == 1 && queued == 1 && inflight == 0 && active == 0) {
                queued = 0;
                owned = 0;
            }
            action AskNewGroup when (requests == 1 && inflight == 1 && active == 1) {
                requests = 2;
                latest = 2;
                queued = if Buggy == 1 { 1 } else { 0 };
            }
            action AskAgain when (requests == 2 && inflight == 1 && active == 1) {
                requests = 3;
                queued = if Buggy == 1 { queued + 1 } else { queued };
            }
            action Cancel when (requests <= 2 && requests > 0 && owned == 1 && active == 1) {
                active = 0;
            }
            action Reenable when (active == 0 && owned == 1 && requests <= 2) {
                requests = 3;
                latest = 3;
                active = 1;
            }
            action Finish when (worker == 1 && inflight == 1) {
                inflight = 0;
                completed = snapshot;
                queued = if active == 1 && latest > snapshot {
                    if Buggy == 1 { queued } else { 1 }
                } else { 0 };
                owned = if active == 1 && latest > snapshot { 1 } else { 0 };
            }
            action Crash when (worker == 1 && owned == 1) {
                worker = 0;
                inflight = 0;
                queued = 0;
            }
            action Restart when (worker == 0) {
                worker = 1;
                queued = if owned == 1 { 1 } else { 0 };
            }

            invariant OneTokenPerSession: queued + inflight <= 1;
            invariant LatestRequestRemainsOwned:
                if worker == 1 && inflight == 0 && active == 1 && latest > completed {
                    queued == 1
                } else { queued <= 1 };
        }
    }
}

/// One Claude footer resolver watch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_footer_watch_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeFooterWatch {
            const Buggy = 0;
            var watch = 0;
            var asked_old = 0;
            var asked_new = 0;
            var old_stopped = 0;
            var retired = 0;

            action AskOld when (asked_old == 0 && retired == 0) {
                asked_old = 1;
                watch = 1;
            }
            action AskNew when (asked_old == 1 && asked_new == 0 && retired == 0) {
                asked_new = 1;
                watch = 2;
            }
            action StopOld when (asked_old == 1 && old_stopped == 0 && retired == 0) {
                old_stopped = 1;
                watch = if watch == 1 || Buggy == 1 { 0 } else { watch };
            }
            action StopSession when (asked_old == 1 && retired == 0) {
                retired = 1;
                watch = if Buggy == 1 { watch } else { 0 };
            }
            action IdleRead when (watch > 0 && retired == 0) {
                watch = watch;
            }

            invariant StaleStopKeepsReplacement:
                if asked_new == 1 && old_stopped == 1 && retired == 0 {
                    watch == 2
                } else { watch <= 2 };
            invariant DormantWatchCannotRead:
                if retired == 1 || (old_stopped == 1 && asked_new == 0) {
                    watch == 0
                } else { watch <= 2 };
        }
    }
}

/// Which model the Claude Code footer names (`aterm_agent::harness::footer`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_footer_model_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeFooterModel {
            const Buggy = 0;
            const Pred = 2;
            const Other = 1;
            var launched = 0;
            var runs = 1;
            var seen = 0;
            var carry = 0;
            var known = 0;
            var flag = 0;
            var card = 0;
            var pinned = 0;
            var resumed = 0;
            var restored = 0;
            var late = 0;
            var shown = 0;
            var synced = 0;

            action Flag when (launched == 0 && flag == 0 && card == 0) {
                flag = 1;
                runs = 1;
                pinned = 1;
                restored = 0;
                synced = 0;
            }
            action LaunchResume when (launched == 0 && resumed == 0 && card == 0) {
                resumed = 1;
                runs = if flag == 1 { 1 } else { Pred };
                pinned = if flag == 1 { 1 } else { 2 };
                restored = if flag == 1 { 0 } else { 1 };
                synced = 0;
            }
            action DrawCard when (launched == 0 && card == 0) {
                card = runs;
                synced = 0;
            }
            action Answer when (launched == 1 && seen <= 3) {
                seen = 1;
                synced = 0;
            }
            action ChooseA when (launched == 1 && seen <= 3) {
                runs = 1;
                seen = 2;
                pinned = 1;
                synced = 0;
            }
            action ChooseB when (launched == 1 && seen <= 3) {
                runs = 2;
                seen = 2;
                pinned = 1;
                synced = 0;
            }
            action ChooseUnread when (launched == 1 && seen <= 3) {
                runs = if runs == 1 { 2 } else { 1 };
                seen = 3;
                pinned = 1;
                synced = 0;
            }
            action Flood when (seen > 0) {
                seen = if Buggy == 1 { 0 } else { seen };
                synced = 0;
            }
            action Clear when (launched == 1 && seen > 0 && known == 1 && resumed == 0) {
                carry = seen;
                seen = 0;
                known = 0;
                synced = 0;
            }
            action Resume when (launched == 1 && resumed <= 1 && known == 1) {
                carry = if seen > 0 { seen } else { if restored == 1 { 1 } else { carry } };
                runs = if pinned == 0 { Pred } else { runs };
                restored = if pinned == 0 { 1 } else { if pinned == 2 { 2 } else { 0 } };
                pinned = if pinned == 0 { 2 } else { pinned };
                seen = 0;
                known = 0;
                resumed = resumed + 1;
                synced = 0;
            }
            action ResumeLate when (launched == 1 && resumed == 0 && known == 1 && card == 0) {
                carry = if seen > 0 { seen } else { carry };
                runs = if pinned == 0 { Pred } else { runs };
                restored = if pinned == 0 { 1 } else { 0 };
                pinned = if pinned == 0 { 2 } else { pinned };
                seen = 0;
                known = 0;
                resumed = 1;
                late = 1;
                synced = 0;
            }
            action Read when (synced == 0) {
                shown = if seen == 1 || seen == 2 {
                    // Buggy=1 voids a readable choice.
                    if Buggy == 1 && seen == 2 { 0 } else { runs }
                } else if seen == 3 {
                    0
                } else if Buggy == 1 && resumed > 0 && (restored == 0 || restored == 2) {
                    // Buggy=1: the resumed conversation's own model, read without the floor
                    // under a pin (or by the floor alone in D), or taking a restore for no pin
                    // at the second resume (or missing D's).
                    if resumed == 2 { Other } else { Pred }
                } else if restored == 1 {
                    // Buggy=1: the resume read as a clear — the process's
                    // carried answer (its starting model), else its card.
                    if Buggy == 1 {
                        if carry == 1 || carry == 2 { 1 } else { card }
                    } else {
                        runs
                    }
                } else if carry > 0 && Buggy == 0 {
                    // The process's own statement, from the session it left.
                    if carry == 3 { 0 } else { runs }
                } else if flag == 1 {
                    1
                } else {
                    card
                };
                launched = 1;
                known = 1;
                synced = 1;
            }

            invariant ShownIsTheRunningModel:
                if synced == 1 { shown == 0 || shown == runs } else { shown <= 2 };
            invariant ChoiceShownAtOnce:
                if synced == 1 && (seen == 1 || seen == 2) {
                    shown == runs
                } else { shown <= 2 };
            invariant ClearKeepsTheModel:
                if synced == 1 && seen == 0 && (restored == 0 || restored == 2) && (carry == 1 || carry == 2) {
                    shown == runs
                } else { shown <= 2 };
            invariant ResumeNamesTheRestoredModel:
                if synced == 1 && seen == 0 && restored == 1 {
                    shown == runs
                } else { shown <= 2 };
        }
    }
}
