// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DECLARED WAKES (`docs/DESIGN-harness-occupancy-2026-10-20.md`).
//!
//! THE BOOK (`WakeBook`): one session, one wake, one waiter and one loop. A wake is declared from
//! the session's own tree, latches its condition's edge, and is told once: by the agent's own
//! `await wake` (an answer written, then heard at the next turn the loop sees), or by a loop's
//! door under `prompt` (a claim, then the one line). A crash mid-write allows one retell, and an
//! answer unheard may be retold once by a door. `st` is the wake's state: 0 none, 1 pending, 2
//! ready, 3 answering, 4 answered, 5 claimed, 6 told, 7 gone (expired, taken or removed). An
//! expiry ends a wake a teller holds too (answering, claimed): the write that ends after it
//! changes nothing in the book, and a line it typed is the claim's one line, since gone is final.
//! What the book cannot judge is its callers': the need and `unless=` at the tell, the loop's
//! guards at a claim, a turn heard.
//!
//! THE ROOM (`WakeRoom`): three sessions and one free build slot, their holds aged a < b < c. A
//! line or an answer that needs a slot reserves it, the oldest waiting hold first; a holder's
//! re-ask is granted against its own reservation and renews nothing; a reservation ends at its
//! lapse (its ten minutes, or its session's tab seen holding a slot).

use aterm_spec::derive::{Liveness, Model};

/// THE BOOK.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn wake_book_model() -> Model {
    aterm_spec::ty_model! {
        WakeBook {
            const Buggy = 0;
            const Prompt = 1;
            const Managed = 0;
            const Hosted = 1;
            scenario Prompt = 0;
            scenario Managed = 1;
            scenario Hosted = 0;
            var st = 0;
            var cond = 0;
            var blip = 0;
            var need = 0;
            var unl = 0;
            var door = 0;
            var brake = 0;
            var ran = 0;
            var fails = 0;
            var parked = 0;
            var answers = 0;
            var typed = 0;
            var foreign = 0;
            var missed = 0;
            var midwrite = 0;
            var lost = 0;
            var late = 0;
            var deaf = 0;
            var sneed = 0;
            var sfrom = 0;

            action DeclareOwn when (st == 0) { st = 1; }
            action DeclareOther {}
            action ForeignAwait {}
            action ForeignTake {}
            action Holds when (st == 1 && cond == 0) { cond = 1; }
            action Fails when (st == 1 && cond == 1) { cond = 0; }
            action Blip when (st == 1 && cond == 0 && blip == 0) { blip = 1; }
            action NeedHolds when (st > 0 && 6 > st && need == 0) { need = 1; }
            action NeedFails when (st > 0 && 6 > st && need == 1) { need = 0; }
            action UnlessHolds when (unl == 0 && (st == 1 || st == 2)) { unl = 1; }
            action Break when (st > 0 && 6 > st && door == 0) { door = 1; }
            action Leave when (door == 1) { door = 0; }
            action BrakeOn when (st > 0 && 6 > st && brake == 0) { brake = 1; }
            action BrakeOff when (brake == 1) { brake = 0; }
            action Expire when (st > 0 && 6 > st) {
                st = if (st == 4) { 6 } else { 7 };
                blip = 0;
            }
            action Rm when (st > 0 && 7 > st) {
                st = 7;
                blip = 0;
            }
            action Take when (st > 0 && 7 > st) {
                st = 7;
                blip = 0;
            }
            action TurnRuns when (st == 4 && ran == 0) { ran = 1; }
            action WriteFails when (st == 3 && fails == 0) {
                st = 2;
                fails = 1;
            }
            action CrashMidWrite when (st == 5 && midwrite == 0) {
                st = if (answers > 0) { 4 } else { 2 };
                typed = typed + 1;
                midwrite = 1;
            }

            action Edge when (st == 1 && (cond == 1 || blip == 1)) {
                st = 2;
                blip = 0;
            }
            action Park when (parked == 0 && (st == 1 || st == 2)) { parked = 1; }
            action RefuseSecond when (parked == 1) {}
            action Answer when (st == 2 && parked > 0 && need == 1 && unl == 0 && deaf == 0) {
                st = 3;
                sneed = need;
                sfrom = st;
            }
            action WriteOk when (st == 3) {
                st = if (Hosted == 1) { 4 } else { 6 };
                answers = answers + 1;
                parked = 0;
                ran = 0;
            }
            action Heard when (st == 4 && ran == 1) { st = 6; }
            action Claim when (
                (st == 2 || (st == 4 && ran == 0)) && need == 1 && unl == 0 && Prompt == 1
                    && Managed == 0 && door == 1 && brake == 0 && Hosted == 1
            ) {
                st = 5;
                sneed = need;
                sfrom = 2;
            }
            action Write when (st == 5) {
                st = 6;
                typed = typed + 1;
            }
            action Release when (st == 5) { st = if (answers > 0) { 4 } else { 2 }; }
            action Reconcile when (st == 5) {
                st = 6;
                typed = typed + 1;
            }
            action Unless when (unl == 1 && (st == 1 || st == 2)) {
                st = 7;
                blip = 0;
            }

            action ParkDeaf when (Buggy == 1 && parked == 0 && st == 2) {
                parked = 1;
                deaf = 1;
            }
            action DeclareFromOutside when (Buggy == 2 && st == 0) {
                st = 1;
                foreign = 1;
            }
            action AwaitFromAnyone when (Buggy == 3 && parked == 0 && (st == 1 || st == 2)) {
                parked = 1;
                foreign = 1;
            }
            action TypeTwice when (Buggy == 4 && st == 6 && typed == 1 && answers == 0) { st = 5; }
            action AnswerThenType when (Buggy == 5 && st == 6 && answers == 1 && late == 0) {
                st = 5;
                late = 1;
            }
            action ParkSecond when (Buggy == 6 && parked == 1) { parked = 2; }
            action TellOnStaleNeed when (
                Buggy == 7 && st == 2 && parked > 0 && need == 0 && unl == 0
            ) {
                st = 3;
                sneed = need;
                sfrom = st;
            }
            action TellExpired when (
                Buggy == 8 && st == 7 && typed == 0 && Prompt == 1 && Hosted == 1
            ) {
                st = 5;
                sneed = need;
                sfrom = st;
            }
            action TypeIntoManaged when (
                Buggy == 9 && (st == 2 || (st == 4 && ran == 0)) && need == 1 && unl == 0
                    && Prompt == 1 && door == 1 && brake == 0 && Hosted == 1
            ) {
                st = 5;
                sneed = need;
                sfrom = 2;
            }
            action TypeUnderObserve when (
                Buggy == 10 && (st == 2 || (st == 4 && ran == 0)) && need == 1 && unl == 0
                    && Managed == 0 && door == 1 && brake == 0 && Hosted == 1
            ) {
                st = 5;
                sneed = need;
                sfrom = 2;
            }
            action LevelOnly when (
                Buggy == 11 && st == 1 && cond == 0 && blip == 0 && missed == 0
            ) {
                missed = 1;
            }
            action ToldBeforeWrite when (Buggy == 12 && st == 3 && fails == 0) {
                st = if (Hosted == 1) { 4 } else { 6 };
                fails = 1;
                lost = 1;
                parked = 0;
                ran = 0;
            }
            action ReleaseAfterTyped when (Buggy == 13 && st == 5 && typed == 0) {
                st = if (answers > 0) { 4 } else { 2 };
                typed = typed + 1;
            }
            action RetellAfterHeard when (
                Buggy == 14 && st == 4 && ran == 1 && need == 1 && unl == 0 && Prompt == 1
                    && Managed == 0 && door == 1 && brake == 0 && Hosted == 1
            ) {
                st = 5;
                late = 1;
                sneed = need;
                sfrom = 2;
            }
            action AnswerTwice when (Buggy == 15 && st == 4 && answers == 1) {
                st = 3;
                sneed = need;
                sfrom = 2;
            }

            invariant OnlyItsOwnTree: foreign == 0;
            invariant OneParkedWait: parked <= 1;
            invariant NeedAtTheTell: answers + typed == 0 || sneed == 1;
            invariant OnlyReadyIsTold: answers + typed == 0 || sfrom == 2;
            invariant OneTypedLine: typed <= 1 + midwrite;
            invariant NoTellAfterHeard: late == 0;
            invariant OneAnswer: answers <= 1;
            invariant NoLostAnswer: lost == 0;
            invariant NoLineIntoAManagedWorker: Managed == 0 || typed == 0;
            invariant NothingTypedUnderObserve: Prompt == 1 || typed == 0;
            invariant NoMissedEdge: missed == 0;
            invariant NoDeafWaiter: deaf == 0;
        }
    }
}

/// `AParkedWaitIsAnswered`, `[]<>(parked == 0 || st == 1 || st > 3 || need == 0 || unl == 1)`:
/// a parked `await wake` on a ready wake whose need holds is answered, its reply written.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(wake_book_model)]
pub fn wake_book_liveness() -> Liveness {
    use aterm_spec::derive::{eq, gt, int, or_, var};
    Liveness {
        name: "AParkedWaitIsAnswered",
        goal: or_(
            or_(
                or_(eq(var("parked"), int(0)), eq(var("st"), int(1))),
                gt(var("st"), int(3)),
            ),
            or_(eq(var("need"), int(0)), eq(var("unl"), int(1))),
        ),
        weak: vec!["Answer", "WriteOk"],
        strong: vec![],
        mutants: vec!["ParkDeaf"],
    }
}

/// THE ROOM.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn wake_room_model() -> Model {
    aterm_spec::ty_model! {
        WakeRoom {
            const Buggy = 0;
            var usable = 1;
            var a_rdy = 0;
            var a_grant = 0;
            var a_told = 0;
            var a_res = 0;
            var a_wait = 0;
            var b_rdy = 0;
            var b_grant = 0;
            var b_told = 0;
            var b_res = 0;
            var b_wait = 0;
            var c_rdy = 0;
            var c_grant = 0;
            var c_told = 0;
            var c_res = 0;
            var c_wait = 0;
            var jumped = 0;
            var renewed = 0;
            var kept = 0;
            var refusedheld = 0;

            action ReadyA when (a_rdy == 0 && a_told == 0) { a_rdy = 1; }
            action ReadyB when (b_rdy == 0 && b_told == 0) { b_rdy = 1; }
            action ReadyC when (c_rdy == 0 && c_told == 0) { c_rdy = 1; }
            action ReserveA when (
                a_rdy == 1 && a_grant == 0 && (a_res == 1 || 1 <= usable - b_res - c_res)
            ) {
                a_grant = 1;
                a_res = 1;
                a_wait = 0;
            }
            action ReserveB when (
                b_rdy == 1 && b_grant == 0 && (b_res == 1 || 1 <= usable - a_res - c_res - a_wait)
            ) {
                b_grant = 1;
                b_res = 1;
                b_wait = 0;
            }
            action ReserveC when (
                c_rdy == 1 && c_grant == 0
                    && (c_res == 1 || 1 <= usable - a_res - b_res - a_wait - b_wait)
            ) {
                c_grant = 1;
                c_res = 1;
                c_wait = 0;
            }
            action RefuseA when (
                a_rdy == 1 && a_grant == 0 && a_res == 0 && 1 > usable - b_res - c_res
            ) {
                a_wait = 1;
            }
            action RefuseB when (
                b_rdy == 1 && b_grant == 0 && b_res == 0 && 1 > usable - a_res - c_res - a_wait
            ) {
                b_wait = 1;
            }
            action RefuseC when (
                c_rdy == 1 && c_grant == 0 && c_res == 0
                    && 1 > usable - a_res - b_res - a_wait - b_wait
            ) {
                c_wait = 1;
            }
            action WriteA when (a_grant == 1) {
                a_grant = 0;
                a_rdy = 0;
                a_told = 1;
            }
            action WriteB when (b_grant == 1) {
                b_grant = 0;
                b_rdy = 0;
                b_told = 1;
            }
            action WriteC when (c_grant == 1) {
                c_grant = 0;
                c_rdy = 0;
                c_told = 1;
            }
            action SkipA when (a_grant == 1) { a_grant = 0; }
            action SkipB when (b_grant == 1) { b_grant = 0; }
            action SkipC when (c_grant == 1) { c_grant = 0; }
            action LapseA when (a_res == 1 && kept == 0) { a_res = 0; }
            action LapseB when (b_res == 1 && kept == 0) { b_res = 0; }
            action LapseC when (c_res == 1 && kept == 0) { c_res = 0; }
            action BuildTakes when (usable == 1 && 1 <= a_res + b_res + c_res) { usable = 0; }
            action SlotFrees when (usable == 0) { usable = 1; }

            action RenewWhileWaited when (
                Buggy == 1 && kept == 0 && 1 <= a_res + b_res + c_res
                    && 1 <= a_wait + b_wait + c_wait
            ) {
                kept = 1;
            }
            action HerdTell when (
                Buggy == 2 && b_rdy == 1 && b_grant == 0 && b_res == 0 && 1 <= usable - a_wait
            ) {
                b_grant = 1;
                b_res = 1;
                b_wait = 0;
            }
            action YoungestFirst when (
                Buggy == 3 && c_rdy == 1 && c_grant == 0 && c_res == 0
                    && 1 <= usable - a_res - b_res
            ) {
                c_grant = 1;
                c_res = 1;
                c_wait = 0;
                jumped = if (1 <= a_wait + b_wait) { 1 } else { jumped };
            }
            action RenewOnAsk when (Buggy == 4 && a_rdy == 1 && a_grant == 0 && a_res == 1) {
                a_grant = 1;
                renewed = 1;
            }
            action CountOwn when (
                Buggy == 5 && a_rdy == 1 && a_grant == 0 && a_res == 1
                    && 1 > usable - a_res - b_res - c_res
            ) {
                a_wait = 1;
                refusedheld = 1;
            }

            invariant OneUnitOneTellWhileReserved: a_res + b_res + c_res <= 1;
            invariant TheOlderHoldFirst: jumped == 0;
            invariant AReservationIsTakenOnce: renewed == 0;
            invariant AHolderIsNeverRefused: refusedheld == 0;
            invariant NoReservationOutlivesItsLapse: kept == 0;
        }
    }
}

/// `TheReservationEnds`, `[]<>(a_res + b_res + c_res == 0)`: no reservation stands for good.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(wake_room_model)]
pub fn wake_room_liveness() -> Liveness {
    use aterm_spec::derive::{add, eq, int, var};
    Liveness {
        name: "TheReservationEnds",
        goal: eq(add(add(var("a_res"), var("b_res")), var("c_res")), int(0)),
        weak: vec!["LapseA", "LapseB", "LapseC"],
        strong: vec![],
        mutants: vec!["RenewWhileWaited"],
    }
}
