// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Retryable admission failures restore only the attempt's own scheduling claims.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sync_retry_model() -> Model {
    aterm_spec::ty_model! {
        AccountSyncRetry {
            const Buggy = 0;
            var scenario = 0; var newer = 0; var done = 0;
            var held = 0; var default_held = 0;
            var read = 1; var key = 1; var login = 1; var cooldown = 1;
            action Scenario when (done == 0 && 4 > scenario) { scenario = scenario + 1; }
            action Supersede when (done == 0 && newer == 0) {
                newer = 1; read = 2; key = 2; login = 2; cooldown = 2;
            }
            action Finish when (done == 0) {
                done = 1;
                held = if Buggy == 7 {0} else {if scenario == 1 || (Buggy == 1 && scenario > 1) {1} else {0}};
                default_held = if scenario == 1 || (Buggy == 2 && scenario > 1) {1} else {0};
                read = if Buggy == 8 {read} else {if scenario > 1 && (newer == 0 || Buggy == 3) {0} else {read}};
                key = if scenario > 1 && (newer == 0 || Buggy == 4) {0} else {key};
                login = if scenario > 1 && (newer == 0 || Buggy == 5) {0} else {login};
                cooldown = if scenario > 1 && (newer == 0 || Buggy == 6) {0} else {cooldown};
            }
            invariant RetryNotHeld: done == 0 || 2 > scenario || (held == 0 && default_held == 0);
            invariant RejectedHeld: done == 0 || 1 > scenario || scenario > 1 || (held == 1 && default_held == 1);
            invariant OwnRetryRestored: done == 0 || 2 > scenario || newer == 1
                || (read == 0 && key == 0 && login == 0 && cooldown == 0);
            invariant NewerClaimRetained: newer == 0 || (read == 2 && key == 2 && login == 2 && cooldown == 2);
            bound RetryDomain: scenario <= 4 && newer <= 1 && done <= 1 && held <= 1 && default_held <= 1
                && read <= 2 && key <= 2 && login <= 2 && cooldown <= 2;
        }
    }
}
