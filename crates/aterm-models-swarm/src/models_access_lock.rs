// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One act of another fleet Mac judged against this Mac's fleet access
/// (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §8). `stored` is what the state file holds:
/// 0 is every file that names no level (missing, corrupt, ambiguous, unknown), 1 read, 2 write,
/// 3 full. `need` is the level the act needs (1..3), `granted` whether the grants admit the
/// driver. The level in force is `stored`, or 1 where it is 0: the lock fails closed. An act is
/// admitted only to a granted driver whose need the level covers: the lock is a ceiling over
/// the grants and never one of them.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fleet_access_lock_model() -> Model {
    aterm_spec::ty_model! {
        FleetAccessLock {
            const Buggy = 0;
            var stored = 0; var need = 1; var granted = 0;
            var admitted = 0; var phase = 0;
            action Stored when (phase == 0 && 3 > stored) { stored = stored + 1; }
            action Need when (phase == 0 && 3 > need) { need = need + 1; }
            action Granted when (phase == 0 && 1 > granted) { granted = granted + 1; }
            action Judge when (phase == 0) {
                admitted = if granted == 1 || Buggy == 3 {
                    if stored == 0 {
                        if Buggy == 1 {1}
                        else if Buggy == 2 {if need <= 2 {1} else {0}}
                        else if Buggy == 4 {0}
                        else if need <= 1 {1} else {0}
                    } else if Buggy == 2 {if need <= stored + 1 {1} else {0}}
                    else if Buggy == 4 {if need + 1 <= stored {1} else {0}}
                    else if need <= stored {1} else {0}
                } else {0};
                phase = 1;
            }
            invariant FailClosed: phase == 0 || stored > 0 || admitted == 0 || need == 1;
            invariant NoOverAdmit: phase == 0 || admitted == 0
                || (stored == 0 && need == 1) || (stored > 0 && need <= stored);
            invariant Ceiling: phase == 0 || admitted == 0 || granted == 1;
            invariant ReadsStay: phase == 0 || granted == 0 || need > 1 || admitted == 1;
            bound AccessBox: stored <= 3 && 1 <= need && need <= 3 && granted <= 1
                && admitted <= 1 && phase <= 1;
        }
    }
}
