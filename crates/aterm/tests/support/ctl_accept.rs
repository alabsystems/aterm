// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retries `aterm ctl` calls while the instance has not yet accepted or is busy.

#![allow(dead_code)]

use std::process::Output;
use std::time::Instant;

/// Whether `out` is the `aterm ctl` client's refusal when the control socket did not accept in time.
pub fn was_never_accepted(out: &Output) -> bool {
    out.status.code() == Some(2)
        && String::from_utf8_lossy(&out.stderr).contains("so none was sent")
}

/// Whether `out` is the instance's busy refusal (`ERR control server busy`).
pub fn was_turned_away(out: &Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).contains(BUSY_PREFIX)
}

/// What every busy refusal leads with (`CONTROL_BUSY_REPLY`'s prefix, which
/// clients match).
const BUSY_PREFIX: &str = "ERR control server busy";

/// Runs `attempt` again while it comes back [`was_never_accepted`], up to the hang detector.
pub fn until_accepted(attempt: impl FnMut() -> Output) -> Output {
    asked_again_while(attempt, was_never_accepted)
}

/// Runs `attempt` again while it comes back [`was_never_accepted`] or [`was_turned_away`], up to the hang detector.
pub fn until_served(attempt: impl FnMut() -> Output) -> Output {
    asked_again_while(attempt, |out| {
        was_never_accepted(out) || was_turned_away(out)
    })
}

/// `attempt` again while `unserved` holds of its answer, up to
/// [`aterm_spec::wait::MUST_HAPPEN`].
fn asked_again_while(
    mut attempt: impl FnMut() -> Output,
    unserved: impl Fn(&Output) -> bool,
) -> Output {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let out = attempt();
        if !unserved(&out) || Instant::now() >= give_up {
            return out;
        }
    }
}
