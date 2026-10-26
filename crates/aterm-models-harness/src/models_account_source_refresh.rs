// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded polling, source changes and elapsed monotonic time. A poll is an attempt,
/// never evidence of a successful read or publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_refresh_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourceRefresh {
            const Buggy = 0;
            var source = 1; var seen = 3; var age = 2;
            var polls = 0; var changes = 0; var checked = 0;
            var due = 0; var required = 0; var before_seen = 3; var before_age = 2;
            action Edit when (2 > changes) {
                source = if source == 1 {2} else {1}; changes = changes + 1; checked = 0;
            }
            action Disappear when (source > 0 && 2 > changes) {
                source = 0; changes = changes + 1; checked = 0;
            }
            action Elapse when (2 > age) { age = age + 1; checked = 0; }
            action Poll when (3 > polls) {
                before_seen = seen; before_age = age;
                required = if source == seen && 2 > age {0} else {1};
                due = if Buggy == 1 && seen == 3 {0} else {
                    if Buggy == 2 && source == 0 {0} else {
                        if Buggy == 3 && source == seen {0} else {
                            if Buggy == 4 {1} else {
                                if source == seen && 2 > age {0} else {1}
                            }
                        }
                    }
                };
                seen = if source == seen && 2 > age { if Buggy == 5 {3} else {seen} }
                    else { if Buggy == 7 {3} else {source} };
                age = if Buggy == 6 {0} else {
                    if source == seen && 2 > age {age} else {0}
                };
                polls = polls + 1; checked = 1;
            }
            invariant FirstOrChanged: checked == 0 || source == before_seen || due == 1;
            invariant DeadlineRetry: checked == 0 || 2 > before_age || due == 1;
            invariant QuietSuppressed: checked == 0 || required == 1 || due == 0;
            invariant AttemptObserved: checked == 0 || due == 0 || seen == source;
            invariant QuietKeepsObservation: checked == 0 || due == 1 || seen == before_seen;
            invariant QuietKeepsDeadline: checked == 0 || due == 1 || age == before_age;
        }
    }
}
