// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Cache reuse and serialization across observed vault context changes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vault_refresh_context_model() -> Model {
    aterm_spec::ty_model! {
        VaultRefreshContext {
            const Buggy = 0;
            var context = 1; var phase = 0; var origin = 0;
            var dirty = 0; var expired = 0; var needed = 0;
            var result = 0; var jobs = 0; var moved = 0;

            action ContextA when ((context == 0 || context == 2)) {
                context = 1; result = 0; moved = 0;
            }
            action ContextB when (2 > context) {
                context = 2; result = 0; moved = 0;
            }
            action Unreadable when (context > 0) {
                context = 0; result = 0; moved = 0;
            }
            action Lookup when (1 == 1) {
                result = if phase == 1 {
                    if Buggy == 2 {1} else if Buggy == 4 {3} else {2}
                } else if context == 0 && ((if Buggy == 3 {0} else {1}) == 1) {4}
                else if phase == 2 && (expired == 0 || Buggy == 7) &&
                    ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1) {3}
                else {1};
                phase = if phase == 1 {1}
                    else if context == 0 && ((if Buggy == 3 {0} else {1}) == 1) {0}
                    else if phase == 2 && (expired == 0 || Buggy == 7) &&
                        ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1) {2}
                    else {1};
                origin = if phase == 1 || (context == 0 && ((if Buggy == 3 {0} else {1}) == 1)) ||
                    (phase == 2 && (expired == 0 || Buggy == 7) &&
                        ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1))
                    {origin} else {context};
                dirty = if phase == 1 {
                    if Buggy == 5 {0} else if (context > origin || origin > context) {1} else {dirty}
                } else if phase == 2 && (expired == 0 || Buggy == 7) &&
                    ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1)
                    {dirty} else {0};
                expired = if phase == 1 ||
                    (phase == 2 && (expired == 0 || Buggy == 7) &&
                        ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1))
                    {expired} else {0};
                jobs = if phase == 1 {if Buggy == 2 {2} else {jobs}}
                    else if context == 0 && ((if Buggy == 3 {0} else {1}) == 1) {0}
                    else if phase == 2 && (expired == 0 || Buggy == 7) &&
                        ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1) {0}
                    else {1};
                needed = if phase == 1 {if (context > origin || origin > context) {1} else {needed}}
                    else if context == 0 && ((if Buggy == 3 {0} else {1}) == 1) {needed}
                    else if phase == 2 && (expired == 0 || Buggy == 7) &&
                        ((context == origin && (dirty == 0 || Buggy == 5)) || Buggy == 1)
                    {needed} else {0};
                moved = 0;
            }
            action Complete when (phase == 1) {
                phase = 2; jobs = 0; expired = 0; result = 0; moved = 0;
            }
            action Expire when (phase == 2 && expired == 0) {
                expired = 1; result = 0; moved = 0;
            }
            action Move when (1 == 1) {
                phase = if Buggy == 6 || phase == 1 {phase} else {0};
                dirty = if Buggy == 6 {dirty} else if phase == 1 {1} else {0};
                jobs = if phase == 1 {jobs} else {0};
                needed = 1; result = 0; moved = 1; expired = 0;
            }

            invariant OneRefresh: jobs <= 1;
            invariant CompleteBeforeReuse: (3 > result || result > 3) || phase == 2;
            invariant SameContext: (3 > result || result > 3) || (context > 0 && context == origin);
            invariant FreshBeforeReuse: (3 > result || result > 3) || expired == 0;
            invariant ObservedChangeInvalidates: (3 > result || result > 3) || needed == 0;
            invariant ReadableBeforeStart: (1 > result || result > 1) || context > 0;
            invariant MovementInvalidates: moved == 0 || phase == 0 || (phase == 1 && dirty == 1);
        }
    }
}
