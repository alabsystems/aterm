// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Fabric bridge spends one replay budget across all late subscribers.

use super::Model;

/// Two always-backlogged sessions, four pages per interval. `Control` is the
/// roster's local and addressed work, which precedes replay. `QueuePriority`
/// represents a halt, local event, addressed message or closure arriving while
/// replay runs; the roster may finish one page then must yield. `FetchA/B`
/// choose the next sid strictly after the last served one and spend one shared page.
/// A pushed opt-in may take one page in its event turn, while a roster round
/// can take the shared remainder after controls. `Buggy=1` permits the old
/// per-session behavior: replay before control, repeated pages for one sid,
/// and more than one interval's pages at once.
/// Tier-1 drives the shipping `SayReplay` selector; the bridge's roster path
/// calls it after its status, topic and outbox work.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn say_replay_budget_model() -> Model {
    crate::ty_model! {
        SayReplayBudget {
            const Cap = 4;
            const Buggy = 0;
            var control = 0;
            var due_a = 4;
            var due_b = 4;
            var spent = 0;
            var last = 1;
            var got_a = 0;
            var got_b = 0;
            var rounds = 0;
            var priority = 0;
            var busy_pages = 0;
            var push = 0;
            var push_pages = 0;

            action Control when (control == 0) {
                control = 1;
            }
            action QueuePriority when (priority == 0) {
                priority = 1;
                busy_pages = 0;
            }
            action HandlePriority when (priority == 1) {
                priority = 0;
                busy_pages = 0;
            }
            action BeginPush when (control == 1 && push == 0) {
                push = 1;
                push_pages = 0;
            }
            action EndPush when (push == 1) {
                push = 0;
                push_pages = 0;
            }
            action FetchA when (due_a > 0 && (spent <= Cap - 1 || Buggy == 1)
                && (control == 1 || Buggy == 1)
                && (priority == 0 || (push == 0 && busy_pages == 0) || Buggy == 1)
                && (push == 0 || push_pages == 0 || Buggy == 1)
                && (last == 1 || due_b == 0 || Buggy == 1)) {
                due_a = due_a - 1;
                spent = spent + 1;
                last = 0;
                got_a = got_a + 1;
                busy_pages = if priority == 1 { busy_pages + 1 } else { busy_pages };
                push_pages = if push == 1 { push_pages + 1 } else { push_pages };
            }
            action FetchB when (due_b > 0 && (spent <= Cap - 1 || Buggy == 1)
                && (control == 1 || Buggy == 1)
                && (priority == 0 || (push == 0 && busy_pages == 0) || Buggy == 1)
                && (push == 0 || push_pages == 0 || Buggy == 1)
                && (last == 0 || due_a == 0 || Buggy == 1)) {
                due_b = due_b - 1;
                spent = spent + 1;
                last = 1;
                got_b = got_b + 1;
                busy_pages = if priority == 1 { busy_pages + 1 } else { busy_pages };
                push_pages = if push == 1 { push_pages + 1 } else { push_pages };
            }
            action Refill when (spent == Cap && rounds <= 1) {
                spent = 0;
                rounds = rounds + 1;
            }

            invariant ControlsFirst: spent == 0 || control == 1;
            invariant SharedBound: spent <= Cap;
            invariant FairRotation: got_a <= got_b + 1 && got_b <= got_a + 1;
            invariant PriorityPageBound: busy_pages <= 1;
            invariant PushPageBound: push_pages <= 1;
        }
    }
}
