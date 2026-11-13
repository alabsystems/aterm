// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    // Revision 1's negative control: `NotHeld` without its sweep.
    let unswept = without(&model, "NotHeld", &is("swept", 1));
    assert!(
        aterm_spec::interp::reachable_states(&unswept)
            .iter()
            .any(|state| !unswept.check_invariant("NotHeldNeedsSweep", state))
    );
}
