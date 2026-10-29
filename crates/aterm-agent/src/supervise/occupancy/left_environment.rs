// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `LeftWrite` (`aterm_models_harness::left_write_model`) that no function here
//! performs. The account's own bind where the real code runs: the door's told line on
//! `FactsState::told`, the line left on `FactsState::leave`, and every look's settle (the draft
//! step's Enter, a read's composer and count, the session's end, a count no look may weigh) on
//! `FactsState::look`, each projected by the Tier-1 bind (`tests/conformance_left.rs`). Proof and
//! test builds only.

/// The session's, the person's and the owner's steps, and the mutants of `LeftWrite`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "PersonSends",
        reason = "The person's Enter on the line left: the session's, which the loop's next read sees as its submit count risen (or, on a host that serves no count, a turn running). The Tier-1 bind raises the count exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "PersonClears",
        reason = "The person clearing the line left from the composer: the session's. The Tier-1 bind clears the composer the next read draws exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "Withdraw",
        reason = "The owner's [harness] occupancy leaving prompt: the owner's, read into the loop's config (Occupancy::grant). The Tier-1 bind asks FactsState::draft_may_go without the grant exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "Regrant",
        reason = "The owner's prompt back in force: the owner's. The Tier-1 bind asks with the grant exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "Ends",
        reason = "The session ending (its tab closed, its program gone): the loop's end (drive_to_end's session_gone). The Tier-1 bind settles it at GoneReleased."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "Recount",
        reason = "The count the line was left at becoming one no look may weigh: a successor loop reading the line back from its predecessor's row (door::left_rows_in, seeded uncounted, FactsState::seed_left) or the loop's host back from an outage (FactsState::recount at Session::ride_out), each host process counting its own submits from 0. The Tier-1 bind does each exactly there, the host's count restarted."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "KeepOnClear",
        reason = "Buggy=1 liveness mutant only; a left line kept for good though its composer is clear and nothing was sent (the Tier-1 bind plants LeftNeverSettles in FactsState::look)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "ReleaseOnLeft",
        reason = "Buggy=2 negative control only; a left line booked untold at its write, as every door did before the slice-2 plan's 2.9b (the Tier-1 bind plants ReleaseOnLeft in FactsState::leave)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "SubmitWithoutPrompt",
        reason = "Buggy=3 negative control only; the policy's draft step sending a line prompt left once prompt is no longer in force (the Tier-1 bind plants SubmitWithoutPrompt in FactsState::draft_may_go)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "ToldOnClear",
        reason = "Buggy=4 negative control only; a left line booked told though it was cleared unsent (the Tier-1 bind plants ToldOnClear in FactsState::look)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LeftWrite",
        action = "ReleaseUncounted",
        reason = "Buggy=5 negative control only; a left line released on a count no look may weigh, so the line the person sent under another host process is typed again (the Tier-1 bind plants ReleaseUncounted in FactsState::look)"
    )
)]
#[doc(hidden)]
pub fn left_write_environment() {}
