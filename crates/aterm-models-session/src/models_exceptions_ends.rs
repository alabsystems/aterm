// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An exception whose reporter has stopped naming it ends, by its own clock,
//! without a second look, and never before that clock has run.

use aterm_spec::derive::{Expr, Liveness, Model, eq, int, or_, var};

/// One digest entry of a level family (`MessageCenter::sync_digest`). `cause` is a ghost: whether
/// the fact still holds (only the reporter's pass reads it). `missed` is "the reporter's last pass
/// did not name the entry", which arms its clock (`missed_at`; `digest_deadline` names the instant
/// it ends); `age` counts the clock in `Ticks` of `RESOLVE_AFTER`. `Miss` and `Name` are looks
/// that skip or name it (a flicker neither ends nor re-posts a fact), `Tick` is time passing,
/// `Settle` the loop's wake at the deadline (`settle_digest`).
///
/// The claim is liveness because the upgrade reporter sends a look only when its rows change: a
/// row that leaves produces one empty look and no second one, so every state stays legal while
/// the entry stands for ever. `AnUnnamedEntryEnds`: once a look has missed an entry, it is named
/// again or ended under nothing but time and the loop's wake; `Miss` and `Name` are not fair.
///
/// `Buggy=1` adds four mutants, each its own action. Two break the liveness: `TwoLooksToEnd` (the
/// first missed look arms no clock; only a second look ends it) and `NobodyCallsSettle` (the clock
/// names its instant and nothing calls the settle). Two break the safety: `EndsBeforeTheClockRan`
/// (two looks milliseconds apart end a flickered fact) and `ClockOutlivesTheEntry` (an ended entry
/// keeps a past deadline, so an idle loop wakes for ever).
/// Tier-1: `aterm-messages/tests/conformance_digest_ends.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exceptions_digest_ends_model() -> Model {
    aterm_spec::ty_model! {
        ExceptionsDigestEnds {
            const Buggy = 0;
            const Ticks = 2;
            var open = 1;
            var cause = 1;
            var missed = 0;
            var age = 0;
            var ended_age = 0;
            var frozen = 0;
            var unwired = 0;

            action CauseEnds when (open == 1 && cause == 1) {
                cause = 0;
            }
            action Miss when (open == 1 && missed == 0) {
                missed = 1;
                age = 0;
            }
            action Name when (open == 1 && cause == 1 && missed == 1) {
                missed = 0;
                age = 0;
            }
            action Tick when (open == 1 && missed == 1 && frozen == 0 && age <= Ticks - 1) {
                age = age + 1;
            }
            action Settle when (open == 1 && missed == 1 && frozen == 0 && unwired == 0 && age == Ticks) {
                open = 0;
                missed = 0;
                ended_age = age;
            }
            action TwoLooksToEnd when (Buggy == 1 && open == 1 && cause == 0 && missed == 0 && frozen == 0) {
                missed = 1;
                frozen = 1;
            }
            action NobodyCallsSettle when (Buggy == 1 && open == 1 && cause == 0 && missed == 0 && unwired == 0) {
                missed = 1;
                age = 0;
                unwired = 1;
            }
            action EndsBeforeTheClockRan when (Buggy == 1 && open == 1 && missed == 1 && age <= Ticks - 1) {
                open = 0;
                missed = 0;
                ended_age = age;
            }
            action ClockOutlivesTheEntry when (Buggy == 1 && open == 1 && missed == 1 && age == Ticks) {
                open = 0;
                ended_age = age;
            }

            invariant NoEarlyEnd: open == 1 || ended_age == Ticks;
            invariant ClockDiesWithTheEntry: missed == 0 || open == 1;
            // Each liveness mutant also trips an invariant: the `--strict-vacuity` audit
            // credits a mutant as a negative control only when an invariant alone falsifies it.
            invariant TheClockNeverFreezes: frozen == 0;
            invariant TheSettleIsWired: unwired == 0;
        }
    }
}

/// `[]<>(missed = 0 \/ open = 0)`: the entry is not stuck missed-and-open for
/// good. Weak fairness of `Tick` (time passes) and `Settle` (the loop takes the
/// wake `digest_deadline` arms); each is load-bearing, which the audit checks.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(exceptions_digest_ends_model)]
pub fn exceptions_digest_ends_liveness() -> Liveness {
    let goal: Expr = or_(eq(var("missed"), int(0)), eq(var("open"), int(0)));
    Liveness {
        name: "AnUnnamedEntryEnds",
        goal,
        weak: vec!["Tick", "Settle"],
        strong: vec![],
        mutants: vec!["TwoLooksToEnd", "NobodyCallsSettle"],
    }
}
