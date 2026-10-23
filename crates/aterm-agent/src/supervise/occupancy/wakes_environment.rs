// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `WakeBook` (`aterm_models_harness::wake_book_model`) that no function here
//! performs. The book's own bind where the real code runs: the declaration on
//! [`super::wakes::WakeBook::declare`], the refusals of a peer outside the tree on
//! [`super::declarer::prove`], an edge on `on_source` and `on_look`, the park on
//! [`super::wakes::ParkBook::park`], the answer on `answer_begin` and `answer_done`, the claim, the
//! write, the release and the reconcile on their own functions, each projected by the Tier-1 bind
//! (`tests/conformance_wakes.rs`). Proof and test builds only.

/// The world, the loop's and the board's steps, and the mutants of `WakeBook`: the levels a look
/// reads, the need and the loop's door and guards the book's callers judge, a turn seen, and each
/// `Buggy` member.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "Holds",
        reason = "A pending wake's level coming to hold (a file there, a pid gone, a machine term met): the world's. The Tier-1 bind sets the level a look reads exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "Fails",
        reason = "That level failing again before a look: the world's. The Tier-1 bind clears the level exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "Blip",
        reason = "A level that held and went between two looks (a file made and removed): the world's edge, which the window's watcher hands to on_source. The Tier-1 bind takes that edge at Edge."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "NeedHolds",
        reason = "A wake's need= coming to hold on the machine: the board's. The book keeps the need and never judges it; its callers (the await handler, a door's claim) judge it at the tell. The Tier-1 bind tells only where the model's need holds."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "NeedFails",
        reason = "That need failing: the board's. The Tier-1 bind tells only where the model's need holds."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "UnlessHolds",
        reason = "An unless= atom coming to hold: the world's. The Tier-1 bind sets the level the next look reads exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "Break",
        reason = "A loop's door opening (a settled break, or an idle point that acts): the loop's, which claims only there. The Tier-1 bind claims only where the model's door is open."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "Leave",
        reason = "That door closing: the loop's. The Tier-1 bind claims only where the model's door is open."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "BrakeOn",
        reason = "A guard holding the loop's line (a person, a brake, the pace): the loop's guards, judged before its claim. The Tier-1 bind claims only where the model's brake is off."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "BrakeOff",
        reason = "That guard lifting: the loop's. The Tier-1 bind claims only where the model's brake is off."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "TurnRuns",
        reason = "A turn of the agent seen run after its answer: the loop's reading, which then tells the book (heard). The Tier-1 bind marks the turn exactly there and hears the answer at Heard."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "ParkDeaf",
        reason = "Buggy=1 liveness mutant only; a park that waits for a new edge though its wake is ready, which ParkBook::park and for_await never do"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "DeclareFromOutside",
        reason = "Buggy=2 negative control only; a wake declared from outside the session's tree, which declarer::prove refuses (the Tier-1 bind plants AwaitFromAnyone)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "AwaitFromAnyone",
        reason = "Buggy=3 negative control only; an await parked from outside the session's tree, which declarer::prove refuses (the Tier-1 bind plants it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "TypeTwice",
        reason = "Buggy=4 negative control only; a door's second line for a wake it told, which WakeBook::claim refuses (the Tier-1 bind plants ClaimTwice)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "AnswerThenType",
        reason = "Buggy=5 negative control only; a door's line for a wake the await told, which WakeBook::claim refuses (the Tier-1 bind plants ClaimTwice)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "ParkSecond",
        reason = "Buggy=6 negative control only; a second park from one session, which ParkBook::park refuses (the Tier-1 bind plants it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "TellOnStaleNeed",
        reason = "Buggy=7 negative control only; an answer whose need no longer holds, which the await handler re-judges before it answers"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "TellExpired",
        reason = "Buggy=8 negative control only; a line for a wake that ended, which WakeBook::claim refuses (Gone)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "TypeIntoManaged",
        reason = "Buggy=9 negative control only; a door's line into a managed worker, which the loop's guards hold (Guard::Managed)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "TypeUnderObserve",
        reason = "Buggy=10 negative control only; a door's line under observe, which only a PromptGrant lets the loop claim"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "LevelOnly",
        reason = "Buggy=11 negative control only; an edge the book misses, judged by level alone (the Tier-1 bind plants LevelOnly in on_source)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "ToldBeforeWrite",
        reason = "Buggy=12 negative control only; an answer booked before its reply was written (the Tier-1 bind plants it in answer_done)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "ReleaseAfterTyped",
        reason = "Buggy=13 negative control only; a successor that releases a claim whose last row says typed (the Tier-1 bind plants it in reconcile_claim)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "RetellAfterHeard",
        reason = "Buggy=14 negative control only; a door's retell of an answer the agent's turn heard, which the loop never claims"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WakeBook",
        action = "AnswerTwice",
        reason = "Buggy=15 negative control only; a second answer to a wake already answered, which WakeBook::answer_begin refuses"
    )
)]
#[doc(hidden)]
pub fn wake_book_environment() {}
