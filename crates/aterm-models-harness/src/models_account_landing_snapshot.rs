// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use aterm_spec::derive::Model;

/// Snapshot checks bracket recording and publishing an account completion.
/// Contexts are unchanged, enriched, replaced, or missing; results are success or
/// failure. A completed transaction is retired even when its publication is stale.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_landing_snapshot_model() -> Model {
    aterm_spec::ty_model! {
        AccountLandingSnapshot {
            const Buggy = 0;
            var phase = 0; var before = 0; var after = 0; var kind = 0; var mode = 0;
            var recorded = 0; var history = 0; var pending = 1; var published = 0;
            var moved = 0; var notified = 0; var struck = 0; var quiet = 1; var polls = 0;
            action Before when (phase == 0 && before <= 2) { before = before + 1; }
            action After when (phase == 0 && after <= 2) { after = after + 1; }
            action Kind when (phase == 0 && kind == 0) { kind = 1; }
            action Mode when (phase == 0 && mode <= 1) { mode = mode + 1; }
            action Record when (phase == 0) {
                phase = 1;
                recorded = if Buggy == 16 && before == 1 {0} else if before <= 1 || Buggy == 1 {1} else {0};
                history = if Buggy == 16 && before == 1 {0} else if before <= 1 || Buggy == 1 {1} else {0};
                struck = if kind == 1 && (before <= 1 || Buggy == 1) {1} else {0};
                pending = if Buggy == 3 {0} else {1};
                moved = if Buggy == 4 {1} else {0};
                quiet = if before <= 1 || Buggy == 9 {0} else {1};
            }
            action Publish when (phase == 1 && recorded == 1) {
                phase = 2;
                pending = if Buggy == 6 && after > 1 {1} else {0};
                published = if Buggy == 17 && after == 1 {0} else if after <= 1 || Buggy == 2 {1} else {0};
                moved = if Buggy == 13 {0} else if kind == 0 && (after <= 1 || Buggy == 2) {1} else {0};
                notified = if kind == 0 && (mode == 1 || Buggy == 5) && (after <= 1 || Buggy == 2) {1} else {0};
                history = if Buggy == 14 {0} else {history};
                quiet = if Buggy == 10 {0} else {1};
            }
            action Poll when (phase == 1 && recorded == 0 && polls == 0) {
                polls = 1;
                quiet = 1;
            }
            action Repair when (phase == 1 && recorded == 0) {
                phase = 3;
                quiet = 1;
            }
            action Retry when (phase == 3) {
                phase = 4;
                recorded = if Buggy == 15 {0} else {1}; history = 1; published = 1;
                pending = if Buggy == 12 {1} else {0};
                moved = if kind == 0 {1} else {0};
                notified = if kind == 0 && mode == 1 {1} else {0};
                struck = if Buggy == 11 {0} else if kind == 1 {1} else {0};
                quiet = 0;
            }
            action Replay when (phase == 2 || phase == 4) {
                phase = 5;
                history = if Buggy == 7 {history + 1} else {history};
                notified = if Buggy == 8 {notified + 1} else {notified};
                quiet = 1;
            }
            invariant RecordPolicy: phase == 0 || phase > 2 || recorded == if before <= 1 {1} else {0};
            invariant BeforePublication: phase == 0 || phase > 1 || (pending == 1 && moved == 0 && notified == 0);
            invariant PublishPolicy: phase == 0 || phase == 1 || phase > 2 || published == if after <= 1 {1} else {0};
            invariant Retirement: phase == 0 || phase == 1 || phase == 3 || pending == 0;
            invariant History: history == recorded;
            invariant Moves: moved == if published == 1 && kind == 0 {1} else {0};
            invariant Notification: notified == if published == 1 && kind == 0 && mode == 1 {1} else {0};
            invariant Failure: struck == if recorded == 1 && kind == 1 {1} else {0};
            invariant Quiet: quiet == if (phase == 1 && recorded == 1) || phase == 4 {0} else {1};
            invariant Progress: phase <= 3 || recorded == 1;
            invariant Once: history <= 1 && notified <= 1;
        }
    }
}
