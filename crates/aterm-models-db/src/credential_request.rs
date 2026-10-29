// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A credential request's life (docs/DESIGN-credential-requests-2026-10-27.md §14.1).

use aterm_spec::derive::Model;

/// A want one Mac raises ends fulfilled, retired with a checkable reason, or awaiting a person;
/// no value reaches a Mac the policy does not give it to; a person is asked only when no Mac can
/// give it. `st`: 0 none, 1 local (the need ledger), 2 open, 3 offered, 4 published,
/// 5 fulfilled, 6 retired, 7 awaiting a person. `why`: 1 never-stored, 2 never-copied,
/// 3 not-authorized, 4 not-needed, 5 not-held, 6 declined, 7 requester-gone, 8 lapsed.
/// `kind`: 0 copyable, 1 nowhere, 2 per Mac, 3 vault-only. `wait`, the one party that moves a
/// non-final state: 1 holders, 2 the custodian, 3 the person, 4 the requester, 5 the founding.
/// `pa`, `askok` and `okr` record, at the step that publishes, asks or raises again, whether
/// the law's condition held then, so a mutant of an honest guard breaks the law, not only a
/// named mutant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn credential_request_model() -> Model {
    aterm_spec::ty_model! {
        DbCredentialRequest {
            const Buggy = 0;
            var st = 0; var why = 0; var kind = 0;
            var commons = 0; var hidden = 0; var person = 0;
            var auth = 1; var gen = 0; var atgen = 0;
            var inv = 0; var held = 0; var swept = 0; var lost = 0; var need = 1;
            var wait = 0; var quiet = 0;
            var posted = 0; var got = 0; var you = 0; var raises = 0;
            var pa = 0; var askok = 0; var okr = 0; var stray = 0; var rogue = 0;

            action Source when (st == 0 && person == 0) { person = 1; }
            action Hidden when (st == 0 && hidden == 0) { hidden = 1; }
            action Raise when (st == 0 && raises == 0) {
                st = 1; raises = 1; wait = if commons == 1 { 4 } else { 5 };
            }
            action RaiseNowhere when (st == 0 && raises == 0) { st = 1; kind = 1; raises = 1; wait = 4; }
            action RaisePerMachine when (st == 0 && raises == 0) { st = 1; kind = 2; raises = 1; wait = 4; }
            action RaiseVaultOnly when (st == 0 && raises == 0) {
                st = 1; kind = 3; person = 1; raises = 1;
                wait = if commons == 1 || hidden == 0 { 4 } else { 5 };
            }
            action CommonsReady when (commons == 0) {
                commons = 1; wait = if st == 1 && wait == 5 { 4 } else { wait };
            }
            action Catalog when (st == 1 && (kind == 1 || kind == 2)) {
                st = 6; why = if kind == 1 { 1 } else { 2 }; wait = 0;
            }
            action Absent when (st == 1 && kind == 3 && commons == 0 && hidden == 0 && inv == 0 && auth == 1) {
                st = 7; wait = 3; you = 1;
                askok = if inv == 0 && ((commons == 0 && hidden == 0) || (swept == 1 && held == 0)) { 1 } else { 0 };
            }
            action Flush when (st == 1 && commons == 1 && (kind == 0 || kind == 3) && auth == 1) {
                st = 2; posted = 1; swept = 0; wait = 1;
            }
            action Post when (st == 7 && posted == 0 && commons == 1) { posted = 1; }
            action Unauthorized when ((st == 1 || st == 2 || st == 7) && (kind == 0 || kind == 3) && auth == 0) {
                st = 6; why = 3; atgen = gen; wait = 0; you = 0;
            }
            action Stored when (inv == 0 && st > 0 && 4 > st) { inv = 1; }
            action Widen when (st == 2 && auth == 1 && inv == 1) { st = 4; got = 1; pa = auth; wait = 4; }
            action Arrive when (held == 0 && st > 0 && (kind == 0 || kind == 3)) { held = 1; }
            action Offer when (st == 2 && auth == 1 && inv == 0 && held == 1) { st = 3; wait = 2; }
            action OfferLost when (st == 3 && lost == 0) { st = 2; held = 0; lost = 1; swept = 0; wait = 1; }
            action Publish when (st == 3 && auth == 1) { st = 4; got = 1; inv = 1; pa = auth; wait = 4; }
            action Revoked when (st == 3 && auth == 0) { st = 6; why = 3; atgen = gen; wait = 0; }
            action Apply when (st == 4) { st = 5; wait = 0; }
            action Sweep when (st == 2 && auth == 1 && inv == 0 && held == 0 && swept == 0) { swept = 1; wait = 2; }
            action NotHeld when (st == 2 && auth == 1 && inv == 0 && held == 0 && swept == 1 && person == 0) {
                st = 6; why = 5; wait = 0;
            }
            action NeedsPerson when (st == 2 && auth == 1 && inv == 0 && held == 0 && swept == 1 && person == 1) {
                st = 7; wait = 3; you = 1;
                askok = if inv == 0 && ((commons == 0 && hidden == 0) || (swept == 1 && held == 0)) { 1 } else { 0 };
            }
            action LateOffer when (st == 7 && posted == 1 && held == 1 && auth == 1) { st = 3; wait = 2; you = 0; }
            action Give when (st == 7 && auth == 1) { st = 4; got = 1; inv = 1; pa = auth; wait = 4; you = 0; }
            action Decline when (st == 7) { st = 6; why = 6; wait = 0; you = 0; }
            action Unneeded when (need == 1 && ((st > 0 && 5 > st) || st == 7)) {
                need = 0; st = 6; why = 4; wait = 0; you = 0;
            }
            action Gone when (posted == 1 && ((st > 1 && 5 > st) || st == 7)) { st = 6; why = 7; wait = 0; you = 0; }
            action Quiet when (posted == 1 && quiet == 0) { quiet = 1; }
            action Lapse when (posted == 1 && quiet == 1 && (st == 2 || st == 7)) { st = 6; why = 8; wait = 0; you = 0; }
            action Policy when (2 > gen) { gen = gen + 1; auth = if auth == 1 { 0 } else { 1 }; }
            action NeedAgain when (st == 6 && why == 4 && need == 0) { need = 1; }
            action Reraise when (st == 6 && need == 1 && raises == 1
                && (why == 4 || why == 8 || (why == 3 && (gen > atgen || atgen > gen)) || (why == 5 && held == 1))) {
                st = 1; why = 0; swept = 0; got = 0; pa = 0; posted = 0; raises = 2; quiet = 0;
                okr = if why == 4 || why == 8 || (why == 3 && (gen > atgen || atgen > gen)) || (why == 5 && held == 1) { 1 } else { 0 };
                wait = if commons == 1 || kind == 1 || kind == 2 || (kind == 3 && hidden == 0) { 4 } else { 5 };
            }

            action BuggyPublishUnauthorized when (Buggy == 1 && st == 3 && auth == 0) { st = 4; got = 1; pa = auth; wait = 4; }
            action BuggyFlushCatalog when (Buggy == 2 && st == 1 && kind == 1 && commons == 1) { st = 2; posted = 1; wait = 1; }
            action BuggyNotHeldUnswept when (Buggy == 3 && st == 2 && held == 0 && swept == 0 && inv == 0) { st = 6; why = 5; wait = 0; }
            action BuggyRetireSilently when (Buggy == 4 && st == 2) { st = 6; wait = 0; }
            action BuggyStrandOffer when (Buggy == 5 && st == 3) { wait = 0; }
            action BuggyReraiseSame when (Buggy == 6 && st == 6 && why == 3 && gen == atgen && raises == 1) {
                st = 1; why = 0; raises = 2; wait = 4;
                okr = if why == 4 || why == 8 || (why == 3 && (gen > atgen || atgen > gen)) || (why == 5 && held == 1) { 1 } else { 0 };
            }
            action BuggyDeclineKeepsNeedsYou when (Buggy == 7 && st == 7) { st = 6; why = 6; wait = 0; }
            action BuggyFulfilledOnOffer when (Buggy == 8 && st == 3) { st = 5; wait = 0; }
            action BuggyPublishToEveryone when (Buggy == 9 && st == 3 && auth == 1) { st = 4; got = 1; inv = 1; pa = auth; wait = 4; stray = 1; }
            action BuggyPublishWithoutCustody when (Buggy == 10 && st == 3 && auth == 1) { st = 4; got = 1; inv = 1; pa = auth; wait = 4; rogue = 1; }
            action BuggyAskPersonForHeld when (Buggy == 11 && st == 2 && held == 1 && inv == 0 && auth == 1) {
                st = 7; wait = 3; you = 1;
                askok = if inv == 0 && ((commons == 0 && hidden == 0) || (swept == 1 && held == 0)) { 1 } else { 0 };
            }
            action BuggyAbsentDespiteHidden when (Buggy == 12 && st == 1 && kind == 3 && commons == 0 && hidden == 1) {
                st = 7; wait = 3; you = 1;
                askok = if inv == 0 && ((commons == 0 && hidden == 0) || (swept == 1 && held == 0)) { 1 } else { 0 };
            }
            action BuggyLapseLive when (Buggy == 13 && posted == 1 && quiet == 0 && (st == 2 || st == 7)) {
                st = 6; why = 8; wait = 0; you = 0;
            }
            action BuggyParkSweep when (Buggy == 1 && st == 2 && auth == 1 && inv == 0 && held == 0 && swept == 0) {
                swept = 2;
            }

            invariant NoUnauthorizedValue: got == 0 || pa == 1;
            invariant CatalogNeverLeaves: kind == 0 || kind == 3 || posted == 0;
            invariant NotHeldNeedsSweep: 5 > why || why > 5 || swept == 1;
            invariant RetiredHasReason: 6 > st || st > 6 || why > 0;
            invariant WaiterNamed: ((st == 0 || st == 5 || st == 6) && wait == 0)
                || (((st > 0 && 5 > st) || st == 7) && wait > 0);
            invariant NoSameReasonReraise: 2 > raises || okr == 1;
            invariant NeedsYouOnlyAwaitingPerson: (you == 0 || st == 7) && (7 > st || you == 1);
            invariant FulfilledGotIt: 5 > st || st > 5 || got == 1;
            invariant NoStrayValue: stray == 0;
            invariant OnlyCustodianPublishes: rogue == 0;
            invariant NoNeedlessAsk: you == 0 || askok == 1;
            invariant LapseOnlyQuiet: 8 > why || why > 8 || quiet == 1;
            invariant SweepDecides: swept <= 1;
            bound StRange: st <= 7; bound WhyRange: why <= 8; bound WaitRange: wait <= 5;
            bound SweptRange: swept <= 2; bound GenRange: gen <= 2; bound RaisesRange: raises <= 2;
        }
    }
}

/// A REQUEST ENDS: every raised want is, infinitely often, fulfilled, retired with its reason or
/// awaiting a person, assuming each party's step is taken once it stays possible, the founding
/// included (`CommonsReady`: the vault admin's window runs a release that founds Commons).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(credential_request_model)]
pub fn credential_request_liveness() -> aterm_spec::derive::Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    aterm_spec::derive::Liveness {
        name: "ARequestEnds",
        goal: or_(
            or_(eq(var("st"), int(0)), eq(var("st"), int(5))),
            or_(eq(var("st"), int(6)), eq(var("st"), int(7))),
        ),
        weak: vec![
            "CommonsReady",
            "Catalog",
            "Flush",
            "Unauthorized",
            "Widen",
            "Offer",
            "Publish",
            "Revoked",
            "Apply",
            "Sweep",
            "NotHeld",
            "NeedsPerson",
        ],
        strong: vec![],
        mutants: vec!["BuggyParkSweep"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_spec::derive::{Expr, eq, gt, int, or_, var};

    /// `e` with every sub-expression spelled as `clause` (by its `Debug` form) put as `TRUE`.
    fn loosen(e: &Expr, clause: &str) -> Expr {
        if format!("{e:?}") == clause {
            return Expr::Bool(true);
        }
        let b = |x: &Expr| Box::new(loosen(x, clause));
        match e {
            Expr::Add(x, y) => Expr::Add(b(x), b(y)),
            Expr::Sub(x, y) => Expr::Sub(b(x), b(y)),
            Expr::Gt(x, y) => Expr::Gt(b(x), b(y)),
            Expr::Le(x, y) => Expr::Le(b(x), b(y)),
            Expr::Eq(x, y) => Expr::Eq(b(x), b(y)),
            Expr::Or(x, y) => Expr::Or(b(x), b(y)),
            Expr::And(x, y) => Expr::And(b(x), b(y)),
            Expr::Iff(x, y) => Expr::Iff(b(x), b(y)),
            Expr::Neq(x, y) => Expr::Neq(b(x), b(y)),
            Expr::If(c, x, y) => Expr::If(b(c), b(x), b(y)),
            other => other.clone(),
        }
    }

    /// A copy of `m` whose `action` guard holds `clause` as `TRUE`.
    fn without(m: &Model, action: &str, clause: &Expr) -> Model {
        let clause = format!("{clause:?}");
        let mut copy = m.clone();
        let target = copy
            .actions
            .iter_mut()
            .find(|a| a.name == action)
            .expect("the action");
        let guard = target.guard.as_ref().expect("a guard");
        let loosened = loosen(guard, &clause);
        assert_ne!(
            format!("{loosened:?}"),
            format!("{guard:?}"),
            "{action} has no `{clause}`"
        );
        target.guard = Some(loosened);
        copy
    }

    /// Each honest guard §14.1 names is load-bearing: a copy with that one clause removed breaks
    /// its law.
    #[test]
    fn each_honest_guard_bears_its_law() {
        let model = credential_request_model();
        let is = |name, n| eq(var(name), int(n));
        let honest = [
            ("Publish", is("auth", 1), "NoUnauthorizedValue"),
            ("Widen", is("auth", 1), "NoUnauthorizedValue"),
            ("Give", is("auth", 1), "NoUnauthorizedValue"),
            ("NeedsPerson", is("held", 0), "NoNeedlessAsk"),
            ("NeedsPerson", is("swept", 1), "NoNeedlessAsk"),
            ("Absent", is("hidden", 0), "NoNeedlessAsk"),
            (
                "Reraise",
                or_(gt(var("gen"), var("atgen")), gt(var("atgen"), var("gen"))),
                "NoSameReasonReraise",
            ),
            ("Reraise", is("held", 1), "NoSameReasonReraise"),
            ("Lapse", is("quiet", 1), "LapseOnlyQuiet"),
        ];
        for (action, clause, law) in honest {
            let weakened = without(&model, action, &clause);
            let broken = aterm_spec::interp::reachable_states(&weakened)
                .iter()
                .any(|state| !weakened.check_invariant(law, state));
            assert!(broken, "{action} without {clause:?} breaks no {law}");
        }
        // Revision 1's negative controls: `NotHeld` without its sweep, `Unauthorized` that
        // keeps the NEEDS YOU up.
        let unswept = without(&model, "NotHeld", &is("swept", 1));
        assert!(
            aterm_spec::interp::reachable_states(&unswept)
                .iter()
                .any(|state| !unswept.check_invariant("NotHeldNeedsSweep", state))
        );
    }
}
