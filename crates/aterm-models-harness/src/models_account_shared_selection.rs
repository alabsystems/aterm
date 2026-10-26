// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An ordinary request can precede shared custody. Resolving it must preserve
/// newer choices and useful existing logins without blocking unrelated work.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_shared_selection_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedSelection {
            const Buggy = 0;
            var phase = 0; var pending = 0; var shared = 0; var ready = 0;
            var active = 0; var tried = 0; var refused = 0; var newer = 0; var progressed = 0;
            var preserved = 1; var workers = 0;
            action Queue when (phase == 0) { phase = 1; pending = 1; }
            action Reserve when (phase == 1) { phase = 2; shared = 1; }
            action TryShared when (phase == 2 && ready == 0 && refused == 0) {
                tried = 1; refused = if Buggy == 1 {0} else {1};
            }
            action Ready when (phase == 2 && ready == 0) { ready = 1; }
            action Replace when (phase == 2 && pending == 1) { pending = 2; newer = 1; }
            action Resolve when (phase == 2) {
                phase = 3;
                pending = if pending == 2 {if Buggy == 2 {0} else {2}}
                    else {if Buggy == 3 && ready == 0 {1} else {0}};
                active = if pending == 1 && ready == 1 {if Buggy == 4 {0} else {1}} else {0};
                preserved = if Buggy == 5 {0} else {preserved};
                workers = if Buggy == 6 {1} else {0};
            }
            action Progress when (phase == 3 && pending == 0 && ready == 0) {
                phase = 4; progressed = if Buggy == 7 {0} else {1};
            }
            invariant RefusesUnsupported: tried == 0 || refused == 1;
            invariant RetainsNewer: newer == 0 || pending == 2;
            invariant NoBlockedQueue: phase <= 2 || pending == 0 || pending == 2;
            invariant UsefulLogin: phase <= 2 || ready == 0 || pending == 2 || active == 1;
            invariant RetainsCustody: preserved == 1;
            invariant NoSharedWorker: workers == 0;
            invariant UsefulProgress: phase <= 3 || progressed == 1;
        }
    }
}
