// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's own step in a quit.
//!
//! The executable that hosts the window or the node job keeps workers of its own in statics
//! (its Commons runtime), which nothing drops: the process ends in `std::process::exit`, or
//! dies of the signal that asked it to quit. What such a worker must write before the process
//! ends, it writes in the step the host installs here. The window's quit runs it after the
//! event loop ended and the sessions closed (`App::end_run`); a terminating signal the process
//! armed for runs it on the orderly-quit thread before the process dies of that signal, in a
//! window (`harness_host::arm_claims_off_as_the_window_dies`) and in the node job
//! (`node_serve`'s `arm_claims_off_on_quit`). The step may so run more than once, and from
//! either thread: a host installs one that is bounded and that a second call finds done. An
//! exit that runs none of these (a signal nothing armed for or a second one, a launch-fatal
//! `_exit`, an update's `exec`) skips the step.

use std::sync::OnceLock;

static STEP: OnceLock<fn()> = OnceLock::new();

/// Install the host's quit step. The first call wins: there is one host.
pub fn install(step: fn()) {
    let _ = STEP.set(step);
}

/// Run the host's quit step, where one is installed.
pub(crate) fn run() {
    if let Some(step) = STEP.get() {
        step();
    }
}
