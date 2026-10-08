// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An exception whose reporter has stopped naming it ENDS, by its own clock,
//! without a second look, and never before that clock has run.

use super::{Expr, Liveness, Model, eq, int, or_, var};

/// One digest entry of a LEVEL family (`MessageCenter::sync_digest`). `cause`
/// is a ghost: whether the fact the entry states still holds (the digest never
/// reads it; only the reporter's pass does). `missed` is "the reporter's last
/// pass did not name the entry", which arms its clock (`missed_at`, and
/// `MessageCenter::digest_deadline` names the instant it ends); `age` counts
/// the clock in `Ticks` of `RESOLVE_AFTER`. `Miss` is a look that does not name
/// it, `Name` a look that does (a flicker neither ends nor re-posts a fact),
/// `Tick` is time passing, `Settle` is the loop's wake at the deadline
/// (`settle_digest`).
///
/// THE CLAIM is a liveness one, and the reason it is stated as one: the upgrade
/// reporter sends a look only when its rows CHANGE, so a row that left in one
/// change produced one empty look and no second one, ever. Every state of that
/// run was legal and had a successor, and the entry stood for ever with the
/// menu-bar dot lit (2026-09-29 review, F1). `AnUnnamedEntryEnds` says that
/// once a look has missed an entry, the entry is named again or ended, under
/// nothing but time passing and the loop taking its wake. Nothing is assumed of
/// the reporter: `Miss` and `Name` are not fair.
///
/// `Buggy=1` adds four mutants, each its own action so none masks another. Two
/// break the liveness: `TwoLooksToEnd` (the second edition's rule: the first
/// missed look arms no clock, only a second look ends the entry) and
/// `NobodyCallsSettle` (the clock runs and names its instant, and nothing wakes
/// the loop or calls the settle: the second edition's 14-day expiry had no
/// caller). Two break the safety around it: `EndsBeforeTheClockRan` (two looks
/// a few milliseconds apart end a fact the reporter merely flickered) and
/// `ClockOutlivesTheEntry` (an ended entry keeps a deadline in the past, so an
/// idle loop wakes for ever).
/// Tier-1: `aterm-messages/tests/conformance_digest_ends.rs` drives the real
/// center in lock step.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exceptions_digest_ends_model() -> Model {
    crate::ty_model! {
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
            // The two liveness mutants are also SAFETY-visible, so each is
            // independently caught by an invariant (the `--strict-vacuity`
            // audit of `aterm-gui`'s `spec_xref_closure` credits a dead action
            // as a negative control only when an invariant alone falsifies it):
            // the clock never stops running, and something always wakes the
            // loop for the instant `digest_deadline` names.
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
