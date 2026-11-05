// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

pub const fn may_release(already_reaped: bool, poll_ok: bool, exit_observed: bool) -> bool {
    if already_reaped {
        true
    } else if poll_ok {
        exit_observed
    } else {
        false
    }
}
