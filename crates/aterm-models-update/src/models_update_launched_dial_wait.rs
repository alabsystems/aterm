// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outgoing process's wait for its launched successor's dial
//! (`docs/DESIGN-warm-successor-2026-09-29.md`, "A successor that dies before its dial").
//!
//! On the launched lane the outgoing process asks LaunchServices for its successor, then
//! waits, readers live, for the rendezvous dial (`app_update_handoff::await_launched_dial`).
//! A successor dials before its `NSApplication` exists, so before it checks in; one that
//! dies first (a containment-deny fatal in the warm prologue, a boot crash, a refused
//! boot-apply re-exec) is answered 26–251 ms after its death as terminated with no pid
//! (`LaunchError::ExitedBeforeCheckIn`). The wait peeks that answer rather than wait out
//! the 30 s dial budget.
//!
//! A process that checks in first is answered with its pid, and LaunchServices says
//! nothing when it dies afterwards; for that answer the wait asks the kernel about the pid
//! (`app_update_handoff::NamedSuccessorWatch`: an exit watch, then pid vacancy or reuse).

use aterm_spec::derive::{Liveness, Model, eq, gt, int, le, or_, var};

/// `NativeUpdateLaunchedDialWait`: the parent's wait for the dial, the successor it waits
/// for, and LaunchServices' one answer to the launch.
///
/// State: `successor` — 0 booting, 1 dialled, 2 dead before its dial. `answer` — 0 none
/// yet, 1 one that proves nothing and names no process (an error, an unknown pid on a
/// running application, this process's own pid), 2 "terminated before it checked in", 3
/// the launched process's pid (at check-in, or at LaunchServices' ~30 s check-in timeout).
/// `slot` — the answer sits untaken in the launch's one-answer slot. `wait` — 0 waiting,
/// 1 a dialer accepted, 2 the dial budget ran out, 3 ended on the successor's death (2
/// and 3 roll back alike: nothing was accepted). `expired` — the budget ran out.
/// `polls_answer` — the accept's abort peeks the answer. `watches_named` — the abort asks
/// the kernel about a named pid. `taken` — the post-accept corroboration took the answer.
///
/// Premises, measured rather than derived:
/// * `AnswerExited` is enabled only for a dead successor: on macOS 26.6.2 a live process
///   that never checked in was answered with its pid, never "terminated" (stand-in
///   bundles, and this build through `ATERM_DEBUG_DIE_BEFORE_DIAL`).
/// * The pid an `AnswerNamed` names is the successor's: LaunchServices names the process
///   it launched (this process's own pid is an `AnswerOther`), and the successor never
///   replaces its image: since 2026-10-19 its parent swaps before the launch, so its
///   boot apply execs nothing. An older build's successor did re-exec, keeping its pid
///   and raising no exit on a watch; LaunchServices then answered it with that pid (an
///   `AnswerNamed`, still the successor's) or, after an exec some hundred milliseconds
///   into the launch, with -1 (an `AnswerOther`). So the kernel's word that the pid is
///   gone (an exit on the watch, or the pid vacant or recycled) is `EndOnNamedExit`'s
///   `successor == 2`.
///
/// `AnswerOther` and `AnswerNamed` are enabled for a live and a dead successor alike: an
/// error is never evidence of death, and a named process may die later.
///
/// Invariants, each caught by its own `Buggy = 1` action:
/// * `NeverRefusesALiveSuccessor` — the wait ends on a death only when the successor is
///   dead. `EndOnAnyAnswer` takes an error for death; `EndOnNamedPid` takes a named pid
///   for gone without the kernel's word (a pid the watch could not register on).
/// * `AnswerKeptForTheCorroboration` — the answer stays in the slot until the
///   post-accept check that refuses a dialer whose pid is not the launched one.
///   `ConsumingPoll` reads it with the consuming `wait` instead of the peek.
/// * `AReportedDeathIsSeen` / `ANamedDeathIsSeen` — while the wait runs, the abort looks at
///   a reported death / asks the kernel about a named pid. Broken by `PollCancelOnly` /
///   `IgnoreNamedPid`, which also break the liveness claim
///   ([`native_update_launched_dial_wait_liveness`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_launched_dial_wait_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateLaunchedDialWait {
            const Buggy = 0;
            var successor = 0;
            var answer = 0;
            var slot = 0;
            var wait = 0;
            var expired = 0;
            var polls_answer = 1;
            var watches_named = 1;
            var taken = 0;

            // The successor: it dials, or it dies first.
            action Dial when (successor == 0) {
                successor = 1;
            }
            action Die when (successor == 0) {
                successor = 2;
            }
            // LaunchServices answers once; "terminated" only for a dead successor.
            action AnswerExited when (answer == 0 && successor == 2) {
                answer = 2;
                slot = 1;
            }
            action AnswerOther when (answer == 0) {
                answer = 1;
                slot = 1;
            }
            // The launched process's pid; it may die afterwards.
            action AnswerNamed when (answer == 0) {
                answer = 3;
                slot = 1;
            }
            // The environment: the dial budget runs out.
            action Expire when (wait == 0 && expired == 0) {
                expired = 1;
            }

            // The outgoing process.
            action Accept when (wait == 0 && successor == 1) {
                wait = 1;
            }
            action TimeOut when (wait == 0 && expired == 1) {
                wait = 2;
            }
            // The accept's abort peeks the answer: it leaves it in the slot.
            action EndOnExit when (wait == 0 && polls_answer == 1 && slot == 1 && answer == 2) {
                wait = 3;
            }
            // …and asks the kernel about a named pid (gone only when the successor is).
            action EndOnNamedExit when (
                wait == 0 && watches_named == 1 && slot == 1 && answer == 3 && successor == 2
            ) {
                wait = 3;
            }
            // After the accept: the one reader that takes the answer.
            action Corroborate when (wait == 1 && slot == 1 && taken == 0) {
                slot = 0;
                taken = 1;
            }

            // Mutants, dead at Buggy = 0.
            action EndOnAnyAnswer when (Buggy == 1 && wait == 0 && slot == 1 && answer == 1) {
                wait = 3;
            }
            action ConsumingPoll when (Buggy == 1 && wait == 0 && slot == 1) {
                slot = 0;
            }
            // (liveness) An abort that asks only the worker's cancel.
            action PollCancelOnly when (Buggy == 1 && wait == 0 && polls_answer == 1) {
                polls_answer = 0;
            }
            // A named pid taken for gone without the kernel's word.
            action EndOnNamedPid when (Buggy == 1 && wait == 0 && slot == 1 && answer == 3) {
                wait = 3;
            }
            // (liveness) An abort that never asks the kernel about a named pid.
            action IgnoreNamedPid when (Buggy == 1 && wait == 0 && watches_named == 1) {
                watches_named = 0;
            }

            invariant NeverRefusesALiveSuccessor: wait <= 2 || successor == 2;
            invariant AnswerKeptForTheCorroboration: answer == 0 || slot == 1 || taken == 1;
            invariant AReportedDeathIsSeen:
                wait > 0 || answer <= 1 || answer > 2 || polls_answer == 1;
            invariant ANamedDeathIsSeen: wait > 0 || answer <= 2 || watches_named == 1;
        }
    }
}

/// [`native_update_launched_dial_wait_model`]'s progress claim: a successor that died
/// before its dial ends the parent's wait (trivially true while it lives, or after
/// LaunchServices answered an error).
///
/// Fairness, each an assumption someone owns:
/// * `AnswerExited` — LaunchServices answers the launch of a process that died before it
///   checked in (measured 26–251 ms after the death);
/// * `EndOnExit` — the accept asks its abort every 10 ms poll slice
///   (`handoff_rendezvous::Rendezvous::accept_claim_observed`);
/// * `EndOnNamedExit` — that abort asks the kernel about a named pid every slice, and the
///   kernel answers for a gone process: its exit is queued on the watch, and launchd reaps
///   it at once, leaving the pid vacant for a watch that missed it.
///
/// `Expire` is not fair: the wait must end without the dial budget. Mutants:
/// `PollCancelOnly` and `IgnoreNamedPid`, each of which waits out the budget.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_launched_dial_wait_model)]
pub fn native_update_launched_dial_wait_liveness() -> Liveness {
    Liveness {
        name: "TheDeadSuccessorEndsTheWait",
        goal: or_(
            gt(var("wait"), int(0)),
            or_(le(var("successor"), int(1)), eq(var("answer"), int(1))),
        ),
        weak: vec!["AnswerExited", "EndOnExit", "EndOnNamedExit"],
        strong: vec![],
        mutants: vec!["PollCancelOnly", "IgnoreNamedPid"],
    }
}
