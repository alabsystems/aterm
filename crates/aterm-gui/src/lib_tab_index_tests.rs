// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::TabIndex;

#[test]
fn switch_to_clamps_to_range() {
    let mut t = TabIndex::new(2, 3); // tabs 0,1,2; active 2
    assert_eq!(t.switch_to(0), 0);
    assert_eq!(t.switch_to(1), 1);
    assert_eq!(t.switch_to(9), 1);
    assert_eq!(t.active, 1);
}

#[test]
fn cycle_wraps_both_directions() {
    let mut t = TabIndex::new(0, 3);
    assert_eq!(t.cycle(true), 1);
    assert_eq!(t.cycle(true), 2);
    assert_eq!(t.cycle(true), 0); // wrap forward 2 → 0
    assert_eq!(t.cycle(false), 2); // wrap backward 0 → 2
    assert_eq!(t.cycle(false), 1);
    let mut one = TabIndex::new(0, 1);
    assert_eq!(one.cycle(true), 0);
    assert_eq!(one.cycle(false), 0);
    assert_eq!(
        one,
        TabIndex {
            active: 0,
            count: 1
        }
    );
}

#[test]
fn close_last_tab_signals_exit() {
    let mut t = TabIndex::new(0, 1);
    assert!(t.close(0), "closing the only tab must signal exit");
    let mut t2 = TabIndex::new(0, 2);
    assert!(!t2.close(5));
    assert_eq!(
        t2,
        TabIndex {
            active: 0,
            count: 2
        }
    );
}

use aterm_models_gui::tab_nav_model;
use aterm_spec::derive::Model;
use std::collections::BTreeMap;

fn project(t: &TabIndex) -> BTreeMap<&'static str, i64> {
    let mut s = BTreeMap::new();
    s.insert("count", t.count as i64);
    s.insert("active", t.active as i64);
    s
}

fn assert_invariants(m: &Model, s: &BTreeMap<&'static str, i64>, ctx: &str) {
    assert!(
        m.check_invariant("CountPositive", s),
        "CountPositive violated {ctx}: {s:?}"
    );
    assert!(
        m.check_invariant("ActiveInRange", s),
        "ActiveInRange violated {ctx}: {s:?}"
    );
}

fn assert_admissible(
    m: &Model,
    action: &str,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
) {
    let succ = m.successors(action, prev);
    assert!(
        succ.iter().any(|s| s == next),
        "real {action} step {prev:?} -> {next:?} is NOT an admissible model transition; \
         model admits {succ:?}"
    );
}

#[test]
fn real_tab_index_conforms_to_ty_proven_model() {
    let m = tab_nav_model();

    let mut t = TabIndex::new(0, 1);
    assert_eq!(
        project(&t),
        m.init_state(),
        "real init must match the model init"
    );
    assert_invariants(&m, &project(&t), "at init");

    for _ in 0..3 {
        let prev = project(&t);
        let opened = t.add();
        assert_eq!(opened, t.count - 1, "add returns the appended tab's index");
        assert_eq!(t.active, opened, "the new tab becomes active");
        let next = project(&t);
        assert_admissible(&m, "NewTab", &prev, &next);
        assert_invariants(&m, &next, "after NewTab");
    }
    assert_eq!(
        t,
        TabIndex {
            active: 3,
            count: 4
        }
    );

    for i in [0usize, 2, 1, 3] {
        let prev = project(&t);
        assert_eq!(t.switch_to(i), i, "an in-range switch lands on its tab");
        let next = project(&t);
        assert_admissible(&m, "SelectTab", &prev, &next);
        assert_invariants(&m, &next, "after SelectTab");
    }

    for _ in 0..5 {
        let prev = project(&t);
        t.cycle(true);
        let next = project(&t);
        assert_admissible(&m, "Cycle", &prev, &next);
        assert_invariants(&m, &next, "after Cycle");
    }
    for _ in 0..5 {
        let prev = project(&t);
        let back = (t.active + t.count - 1) % t.count;
        assert_eq!(t.cycle(false), back, "a backward cycle wraps 0 -> last");
        let next = project(&t);
        assert_admissible(&m, "SelectTab", &prev, &next);
        assert_invariants(&m, &next, "after Cycle(backward)");
    }

    while t.count > 1 {
        t.switch_to(t.count - 1); // active is the last tab
        let prev = project(&t);
        let exit = t.close(t.active);
        assert!(!exit, "closing a non-last tab never signals exit");
        let next = project(&t);
        assert_admissible(&m, "Close", &prev, &next);
        assert_invariants(&m, &next, "after Close(active==last)");
    }
    assert_eq!(
        t,
        TabIndex {
            active: 0,
            count: 1
        }
    );

    t.add();
    t.add(); // 3 tabs, active 2
    t.switch_to(2);
    let prev = project(&t);
    let exit = t.close(0); // earlier tab: active shifts 2 -> 1
    assert!(!exit);
    let next = project(&t);
    assert_eq!(
        t,
        TabIndex {
            active: 1,
            count: 2
        }
    );
    assert_admissible(&m, "Close", &prev, &next);
    assert_invariants(&m, &next, "after Close(earlier tab)");

    t.add(); // 3 tabs, active 2
    t.switch_to(0);
    let prev = project(&t);
    assert!(!t.close(2));
    assert_eq!(
        t,
        TabIndex {
            active: 0,
            count: 2
        },
        "closing after the active tab leaves it where it is"
    );
    assert_admissible(&m, "Close", &prev, &project(&t));
    t.add(); // 3 tabs, active 2
    t.switch_to(1);
    let prev = project(&t);
    assert!(!t.close(1));
    assert_eq!(
        t,
        TabIndex {
            active: 1,
            count: 2
        },
        "closing the active middle tab keeps its index"
    );
    assert_admissible(&m, "Close", &prev, &project(&t));
    assert_invariants(&m, &project(&t), "after Close(active middle)");

    let mut bad = project(&t);
    bad.insert("active", bad["count"]); // active == count: one past the last tab
    assert!(
        !m.check_invariant("ActiveInRange", &bad),
        "ActiveInRange must REJECT active==count (else the conformance is vacuous)"
    );

    let last_focused = TabIndex {
        active: 2,
        count: 3,
    }; // active is the last tab
    let prev = project(&last_focused);
    let buggy_next = {
        let mut s = prev.clone();
        s.insert("count", 2); // count-- , but active NOT re-clamped (stays 2)
        s
    };
    assert!(
        !m.check_invariant("ActiveInRange", &buggy_next),
        "the forgot-to-reclamp result MUST violate ActiveInRange"
    );
    assert!(
        !m.successors("Close", &prev)
            .iter()
            .any(|s| s == &buggy_next),
        "the buggy unclamped Close is NOT an admissible transition of the correct model"
    );
    let mut real = last_focused;
    assert!(!real.close(2));
    let real_next = project(&real);
    assert_admissible(&m, "Close", &prev, &real_next);
    assert_invariants(&m, &real_next, "real close re-clamps (no bug)");
}
