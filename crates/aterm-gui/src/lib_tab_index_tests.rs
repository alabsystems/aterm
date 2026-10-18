// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::TabIndex;

/// Cmd-1..9: switch to an existing tab; out-of-range is a no-op.
#[test]
fn switch_to_clamps_to_range() {
    let mut t = TabIndex::new(2, 3); // tabs 0,1,2; active 2
    assert_eq!(t.switch_to(0), 0);
    assert_eq!(t.switch_to(1), 1);
    // Out of range (Cmd-5 in a 3-tab window): no change.
    assert_eq!(t.switch_to(9), 1);
    assert_eq!(t.active, 1);
}

/// Cmd-Shift-]/[ cycles with WRAP; single/zero tab is a no-op.
#[test]
fn cycle_wraps_both_directions() {
    let mut t = TabIndex::new(0, 3);
    assert_eq!(t.cycle(true), 1);
    assert_eq!(t.cycle(true), 2);
    assert_eq!(t.cycle(true), 0); // wrap forward 2 → 0
    assert_eq!(t.cycle(false), 2); // wrap backward 0 → 2
    assert_eq!(t.cycle(false), 1);
    // One tab: cycling is a no-op in either direction.
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

/// Closing the last tab signals exit (returns true) without mutating state.
#[test]
fn close_last_tab_signals_exit() {
    let mut t = TabIndex::new(0, 1);
    assert!(t.close(0), "closing the only tab must signal exit");
    // Out-of-range close is a no-op that does not signal exit.
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

// Tier-1 conformance: bind the real `TabIndex` to `tab_nav_model()`. After each real
// mutator, project `(active, count)` onto the model and check both invariants, and check
// the real step is one of the matching action's successors.
use aterm_models_gui::tab_nav_model;
use aterm_spec::derive::Model;
use std::collections::BTreeMap;

/// Project a real `TabIndex` onto the model's `{ count, active }` variables.
fn project(t: &TabIndex) -> BTreeMap<&'static str, i64> {
    let mut s = BTreeMap::new();
    s.insert("count", t.count as i64);
    s.insert("active", t.active as i64);
    s
}

/// Assert both model invariants hold on a (projected) state.
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

/// Assert the real `prev -> next` step is one the model's `action` admits (the
/// real next-state is in the action's existential fan-out). This is the
/// transition-level conformance: the real code only ever moves the way the
/// ty-proven model says it may.
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

    // Start from a fresh window (one active tab) — the model's init state.
    let mut t = TabIndex::new(0, 1);
    assert_eq!(
        project(&t),
        m.init_state(),
        "real init must match the model init"
    );
    assert_invariants(&m, &project(&t), "at init");

    // A scripted run touching every action: NewTab x3, SelectTab, Cycle, and
    // Close in the worst-case (active-is-last) shape that stresses the re-clamp.
    // After each real mutator: invariants hold and the step is admissible.

    // NewTab x3 -> 4 tabs (Cap), active follows to the new last each time
    // (open-and-switch: `add` returns the new tab's index and makes it active).
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

    // SelectTab: jump to an in-range index (Cmd-1..9). The model's SelectTab is
    // nondeterministic (`active' \in 0..count-1`), so the real in-range landing
    // must be one admissible successor.
    for i in [0usize, 2, 1, 3] {
        let prev = project(&t);
        assert_eq!(t.switch_to(i), i, "an in-range switch lands on its tab");
        let next = project(&t);
        assert_admissible(&m, "SelectTab", &prev, &next);
        assert_invariants(&m, &next, "after SelectTab");
    }

    // Cycle forward with wrap (Cmd-Shift-]): from each position incl. the wrap.
    for _ in 0..5 {
        let prev = project(&t);
        t.cycle(true);
        let next = project(&t);
        assert_admissible(&m, "Cycle", &prev, &next);
        assert_invariants(&m, &next, "after Cycle");
    }
    // Cycle BACKWARD (Cmd-Shift-[) wraps the other way. The model has no
    // backward action; a backward step is an in-range jump, so it must be an
    // admissible `SelectTab` — and it must land exactly one tab back.
    for _ in 0..5 {
        let prev = project(&t);
        let back = (t.active + t.count - 1) % t.count;
        assert_eq!(t.cycle(false), back, "a backward cycle wraps 0 -> last");
        let next = project(&t);
        assert_admissible(&m, "SelectTab", &prev, &next);
        assert_invariants(&m, &next, "after Cycle(backward)");
    }

    // Close in the worst case for the range invariant: focus the last tab, then
    // close it — the active index must re-clamp down to the new last. Repeat
    // until one tab remains (the model's Close is guarded `count > 1`).
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

    // Also exercise Close of an earlier tab (active shifts down): build back up,
    // then close index 0 while active is later. The model's `Close` clamp covers
    // this too (active <= count-2 stays in range after the shrink).
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

    // Close a later tab (active unchanged) and the active middle tab (active keeps its
    // index, now the tab that shifted in).
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

    // Negative controls: `ActiveInRange` rejects an out-of-range state, so the asserts
    // above are not vacuous.
    let mut bad = project(&t);
    bad.insert("active", bad["count"]); // active == count: one past the last tab
    assert!(
        !m.check_invariant("ActiveInRange", &bad),
        "ActiveInRange must REJECT active==count (else the conformance is vacuous)"
    );

    // 2. The defect the model catches IS the defect the real `close` avoids:
    //    reproduce the buggy "forgot to re-clamp" Close by hand (decrement count
    //    but leave active at the old last index) and show it lands exactly on
    //    the state the model's invariant rejects — and is not an admissible
    //    `Close` of the correct model (so the real clamp is load-bearing).
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
    // The real close on the same state re-clamps and DOES conform.
    let mut real = last_focused;
    assert!(!real.close(2));
    let real_next = project(&real);
    assert_admissible(&m, "Close", &prev, &real_next);
    assert_invariants(&m, &real_next, "real close re-clamps (no bug)");
}
