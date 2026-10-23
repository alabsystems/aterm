// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of `status.toml` package pass stamps and their scheduler readers.

use aterm_spec::derive::*;

/// Formal model of `status.toml` package pass stamps (`last_success_at`, `last_pass_at`, `updated_at`) and their scheduler readers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_pass_stamps_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgPassStamps {
            const Buggy = 0;
            const MaxT = 7;
            var t = 0;
            var succ = 0;
            var fail = 0;
            var upd = 0;
            var pass_end = 0;
            var pass_failed = 0;
            var own_fail = 0;
            var reached_since_own = 0;
            var wait = 0;
            var reached_since_wait = 0;

            action PassReached when (t <= MaxT - 1) {
                t = t + 1;
                succ = t + 1;
                fail = 0;
                upd = t + 1;
                pass_end = t + 1;
                pass_failed = 0;
                reached_since_own = 1;
                reached_since_wait = 1;
            }
            action PassUnreached when (t <= MaxT - 1) {
                t = t + 1;
                succ = if Buggy == 1 { t + 1 } else { succ };
                fail = if Buggy == 1 { fail } else { t + 1 };
                upd = t + 1;
                pass_end = t + 1;
                pass_failed = 1;
            }
            action OwnPassUnreached when (t <= MaxT - 1) {
                t = t + 1;
                fail = t + 1;
                upd = t + 1;
                pass_end = t + 1;
                pass_failed = 1;
                own_fail = t + 1;
                reached_since_own = 0;
            }
            action OtherWrite when (t <= MaxT - 1) {
                t = t + 1;
                upd = t + 1;
            }
            action Wait when (wait == 0 && t <= MaxT - 1) {
                t = t + 1;
                wait = t + 1;
                reached_since_wait = 0;
            }

            invariant FailedMeansTheLastPassFailed:
                (if Buggy == 1 {
                    if upd > succ { 1 } else { 0 }
                } else {
                    if fail > succ { 1 } else { 0 }
                }) == pass_failed;
            invariant SpacingCountsOnlyFullPasses:
                (if Buggy == 1 {
                    upd
                } else {
                    if succ > fail { succ } else { fail }
                }) == pass_end;
            invariant HealedOnlyByAReachedPass:
                own_fail == 0 || succ <= own_fail || reached_since_own == 1;
            invariant StandDownOnlyBehindAReachedPass:
                wait == 0 || succ <= wait || reached_since_wait == 1;
        }
    }
}
