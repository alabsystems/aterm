// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An `aterm ctl` call a live test makes is ASKED AGAIN while the instance has
//! not yet accepted it, for up to [`aterm_spec::wait::MUST_HAPPEN`].
//!
//! `aterm ctl` gives the accept phase 10 s (`aterm-ctl`'s `ACCEPT_DEADLINE`,
//! which `--timeout` can only shorten) and then exits 2 with "no answer from
//! <sock> within 10s: the instance took no request, so none was sent". That is
//! a person's budget: a quick, named failure instead of a silent wait. On a
//! loaded machine a live instance can miss those 10 s, and a correctness test's
//! wait for a listener that must answer is a one-minute hang detector, never a
//! budget (AGENTS.md). Nothing was sent, so asking again cannot run anything
//! twice; any other answer, success or failure, is returned at once.
//!
//! THE INSTANCE CAN ALSO ANSWER BUSY. The control socket answers `ERR control
//! server busy; retry` (`aterm_types::control_verbs::CONTROL_BUSY_REPLY`) to a
//! connection it cannot admit (every request lane at work, or its connection
//! cap reached) and to a wait none of its lanes may take, and the protocol
//! says nothing was served and the request may be sent again: on a machine at
//! a load of 80, a `status` poll of a host_live test met it once in 49 runs and
//! failed the test. That is the same moment as a missed accept, so
//! [`until_served`] asks again on either, up to the same hang detector.
//!
//! Every `aterm ctl` call a live test makes is wrapped in [`until_served`],
//! except where the busy line is itself the answer under test
//! (`control_lanes_live_headless.rs`, which asks through [`until_accepted`]).
//! A test crate that includes `support/headless_boot.rs` includes this module
//! as `ctl_accept` beside it, and most call sites reach it through
//! `headless_boot`'s re-export (`headless_boot::until_served`, and
//! `headless_boot::ctl`, which goes through it); a crate without
//! `headless_boot` (`control_lanes_live_headless.rs`) includes it directly.

#![allow(dead_code)]

use std::process::Output;
use std::time::Instant;

/// Whether `out` is the `aterm ctl` client's refusal to send anything: the
/// instance's control socket did not ACCEPT its connection within the client's
/// own 10 s, and the client says "so none was sent", exit 2. The instance is
/// alive and not yet taking connections (a debug build on a machine under a
/// build storm can be slow to answer), and nothing reached it.
pub fn was_never_accepted(out: &Output) -> bool {
    out.status.code() == Some(2)
        && String::from_utf8_lossy(&out.stderr).contains("so none was sent")
}

/// Whether `out` is the instance's busy refusal, as `aterm ctl` reports it: a
/// failed exit whose stderr carries `ERR control server busy` (the client's
/// prefix match, `aterm-ctl`'s `is_busy`). The instance served nothing.
pub fn was_turned_away(out: &Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).contains(BUSY_PREFIX)
}

/// What every busy refusal leads with (`CONTROL_BUSY_REPLY`'s prefix, which
/// clients match).
const BUSY_PREFIX: &str = "ERR control server busy";

/// Run `attempt` (one `aterm ctl` call, a fresh client each time) again for as
/// long as it comes back [`was_never_accepted`], up to the hang detector.
/// Nothing was sent, so asking again is waiting for the instance to be ready,
/// not retrying an effect; any other answer, success or failure, a busy
/// refusal included, is returned at once.
pub fn until_accepted(attempt: impl FnMut() -> Output) -> Output {
    asked_again_while(attempt, was_never_accepted)
}

/// [`until_accepted`], asked again too while the instance answers busy
/// ([`was_turned_away`]): neither served anything, so asking again waits for
/// the instance, never repeats an effect. Any other answer, success or
/// failure, is returned at once; a refusal that outlasts the hang detector is
/// returned as it stands, for the caller to fail on.
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
