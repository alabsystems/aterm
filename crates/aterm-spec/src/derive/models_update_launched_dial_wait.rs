// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OUTGOING PROCESS'S WAIT FOR ITS LAUNCHED SUCCESSOR'S DIAL
//! (2026-09-29; `docs/DESIGN-warm-successor-2026-09-29.md`, "A successor that
//! dies before its dial").
//!
//! On the launched lane the outgoing process asks LaunchServices for its
//! successor and then waits, every reader live, for the successor's rendezvous
//! dial (`aterm-gui`'s `app_update_handoff::await_launched_dial`). A successor
//! that DIES BEFORE THAT DIAL — a containment-deny fatal inside the warm
//! prologue, a boot crash, a boot apply whose re-exec was refused — used to be
//! seen only when the dial budget ran out, 30 s on. LaunchServices knows
//! sooner: a launched process that exits before it checks in (and a successor
//! dials before its `NSApplication` exists, so before it checks in) is
//! answered, 26–251 ms after its death, with an application that has
//! terminated and has no pid (measured; `app_launch_successor`'s
//! `LaunchError::ExitedBeforeCheckIn`). The wait now peeks that answer.
//!
//! A process that checks in FIRST is answered with its pid, and LaunchServices
//! says nothing more when it dies afterwards (measured on stand-in bundles).
//! For that answer the wait asks the kernel about the named pid instead
//! (`app_update_handoff::NamedSuccessorWatch`: an exit watch, then pid
//! vacancy or recycling).

use super::{Liveness, Model, eq, gt, int, le, or_, var};

/// `NativeUpdateLaunchedDialWait`: the parent's wait for the dial, the
/// successor it waits for, and LaunchServices' one answer to the launch.
///
/// State: `successor` — 0 alive and booting toward its dial, 1 dialled, 2 dead
/// before its dial. `answer` — LaunchServices' answer: 0 none yet, 1 one that
/// proves nothing and names no process (an error, an unknown pid on an
/// application still running, this process's own pid), 2 "the application
/// terminated before it checked in", 3 the launched process's pid (at its
/// check-in, or at LaunchServices' own ~30 s check-in timeout for a live
/// process that never checked in). `slot` — the answer sits in the launch's
/// one-answer slot, untaken. `wait` — 0 waiting, 1 a dialer accepted, 2 the
/// dial budget ran out, 3 ended on the successor's death; 2 and 3 roll back
/// the same way (`NeverTransferred`: nothing was accepted, no descriptor
/// left). `expired` — the budget has run out. `polls_answer` — the accept's
/// abort peeks the answer (0 is the retired abort, which asked only the
/// worker's cancel). `watches_named` — the abort asks the kernel about a
/// named pid (0 is the abort before 2026-09-29's follow-up, which read only
/// LaunchServices' "terminated"). `taken` — the corroboration after the
/// accept took the answer.
///
/// THE PREMISES, stated where they are assumed:
///
/// * `AnswerExited` is enabled only for a successor that is dead. That is
///   LaunchServices' behaviour as measured on macOS 26.6.2 (stand-in bundles
///   that exit or `SIGKILL` themselves before checking in, with and without a
///   same-pid re-exec first, and this build's own successor through the
///   `ATERM_DEBUG_DIE_BEFORE_DIAL` seam), not something the model derives; a
///   live process that never checked in was answered with its pid, not
///   terminated.
/// * The pid an `AnswerNamed` names IS the successor's: LaunchServices names
///   the process it launched (this process's own pid is filtered out as a
///   substitution, and is an `AnswerOther`), and the successor keeps its pid
///   across the boot apply's re-exec (a same-pid `execve`). So the kernel's
///   word that the named pid is gone — its exit on a watch, which a same-pid
///   `execve` does not raise (measured), or the pid vacant or recycled — is
///   `EndOnNamedExit`'s `successor == 2`.
///
/// `AnswerOther` and `AnswerNamed` are enabled for a live successor and a dead
/// one alike: an error is never evidence of death here, and a named process
/// may die after it is named.
///
/// Invariants, each caught by its own `Buggy = 1` action, so none masks
/// another:
///
/// * `NeverRefusesALiveSuccessor` — the wait ends on a death only when the
///   successor is dead. `EndOnAnyAnswer` ends it on an answer that proves
///   nothing: an error taken for death, which would roll back a successor that
///   is alive and about to dial. `EndOnNamedPid` ends it on a named pid
///   without the kernel's word — a pid the watch could not be registered on
///   (a process this one may not watch, or a zombie) taken for gone.
/// * `AnswerKeptForTheCorroboration` — whatever LaunchServices answered stays
///   in the slot until the corroboration after the accept takes it (the check
///   that refuses a dialer whose pid is not the launched one). `ConsumingPoll`
///   reads it before the dial with the consuming `wait` instead of the peek.
/// * `AReportedDeathIsSeen` — while the wait runs, a death LaunchServices has
///   reported is one the accept's abort looks at. `PollCancelOnly`, the retired
///   abort that asked only the worker's cancel, breaks it (and the liveness
///   claim below, which is what it costs: the budget).
/// * `ANamedDeathIsSeen` — while the wait runs, a pid LaunchServices named is
///   one the abort asks the kernel about. `IgnoreNamedPid`, the abort before
///   the follow-up, breaks it (and the liveness claim below).
///
/// Liveness (`TheDeadSuccessorEndsTheWait`,
/// [`native_update_launched_dial_wait_liveness`]): a successor that died before
/// its dial ends the wait WITHOUT the dial budget — whether LaunchServices
/// reports the death or had named the successor before it — unless
/// LaunchServices had answered something that names nothing. Mutants:
/// `PollCancelOnly` and `IgnoreNamedPid`, each of which waited out the budget.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_launched_dial_wait_model() -> Model {
    crate::ty_model! {
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
            // LaunchServices answers once. "Terminated before it checked in"
            // only for a successor that is dead (the premise above).
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
            // …and asks the kernel about a named pid: gone only when the
            // successor is (the premise above).
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
            // (liveness) The retired abort: the worker's cancel and nothing else.
            action PollCancelOnly when (Buggy == 1 && wait == 0 && polls_answer == 1) {
                polls_answer = 0;
            }
            // A named pid taken for gone without the kernel's word.
            action EndOnNamedPid when (Buggy == 1 && wait == 0 && slot == 1 && answer == 3) {
                wait = 3;
            }
            // (liveness) The abort before the follow-up: it never asked the
            // kernel about a named pid.
            action IgnoreNamedPid when (Buggy == 1 && wait == 0 && watches_named == 1) {
                watches_named = 0;
            }

            invariant NeverRefusesALiveSuccessor: wait <= 2 || successor == 2;
            invariant AnswerKeptForTheCorroboration: answer == 0 || slot == 1 || taken == 1;
            // While the wait runs, a reported death is one its abort looks at.
            invariant AReportedDeathIsSeen:
                wait > 0 || answer <= 1 || answer > 2 || polls_answer == 1;
            // While the wait runs, a named pid is one its abort asks about.
            invariant ANamedDeathIsSeen: wait > 0 || answer <= 2 || watches_named == 1;
        }
    }
}

/// [`native_update_launched_dial_wait_model`]'s progress claim: a successor
/// that died before its dial ends the parent's wait — the goal holds once the
/// wait has ended, and trivially while the successor lives or after
/// LaunchServices answered something that names nothing (an error).
///
/// Fairness, each an assumption someone owns:
///
/// * `AnswerExited`, weakly fair — LaunchServices answers the launch of a
///   process that died before it checked in (measured: 26–251 ms after the
///   death);
/// * `EndOnExit`, weakly fair — the accept asks its abort between every 10 ms
///   poll slice (`handoff_rendezvous::Rendezvous::accept_claim`);
/// * `EndOnNamedExit`, weakly fair — that abort asks the kernel about a named
///   pid on every slice, and the kernel answers for a process that is gone:
///   its exit is queued on the watch inside `proc_exit`, and launchd reaps it
///   at once, which leaves the pid vacant for one the watch missed.
///
/// `Expire` is NOT fair: the point is that the wait ends without the dial
/// budget, so the budget is the environment's, and assumed nothing.
///
/// The mutants that break it, each alone: `PollCancelOnly` — the abort the
/// accept had before, which asked only the worker's cancel, so a dead
/// successor was waited for until the budget ran out; and `IgnoreNamedPid` —
/// the abort before the follow-up, which read LaunchServices' "terminated" and
/// nothing about a pid it had named.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
