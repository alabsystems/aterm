// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the `#[refines]` anchor emitter: the fn-entry probe it
//! injects keeps every awkward fn shape's behaviour and records the anchor id.
//! The emitted records reference `::aterm_spec_anchor::{registry,xref}::…`, so this
//! test links `aterm-spec-anchor` and `aterm-spec` (dev-dependencies; the cycle is
//! dev-only and Cargo-legal).

use aterm_spec_macros::refines;

// ── EXECUTION EVIDENCE: the fn-entry probe `#[refines]` injects ──────────────
//
// The emitter no longer leaves the annotated fn untouched: it prepends
// `::aterm_spec_anchor::xref::note_entered("<machine>::<action> @ <fn>")` to the body and
// records that same id on the submitted `RefinementAnchor`. These tests pin the two
// properties that make that safe to do to ~190 anchors across the tree: the
// annotated function's BEHAVIOUR is unchanged for every awkward fn shape, and the
// probe actually fires (so a gate can tell a live seam from a stub).
//
// Thread-locality is what makes the assertions below exact: each `#[test]` runs on
// its own thread, so one test's entries are invisible to another even though the
// harness runs them in parallel.

use aterm_spec::xref::{
    entered_anchor_count, entered_anchor_ids, refinements, reset_entered_anchors, window_is_armed,
};

/// Open an evidence window, failing loudly if the reset did not take.
///
/// `reset_entered_anchors` returns whether the window is genuinely open precisely so
/// this cannot be written as a bare call that silently no-ops: a no-op reset leaves
/// the previous window's entries in place and every assertion below would pass on
/// stale evidence.
///
/// The follow-up check reads `entered_anchor_count`, NOT
/// `entered_anchor_ids().is_empty()`. The latter was vacuous here for the same reason
/// it was vacuous in `StepEvidence::step`: `entered_anchor_ids` returns an empty set
/// on a destroyed TLS slot and on an outstanding borrow, so the assertion held in
/// exactly the two states it claimed to detect. `Some(0)` is an observation; `None`
/// is a failure to observe, and fails.
fn open_window() {
    assert!(
        reset_entered_anchors(),
        "reset_entered_anchors() must clear the record AND arm the probe"
    );
    assert_eq!(
        entered_anchor_count(),
        Some(0),
        "reset must leave this thread's record OBSERVABLY empty — `None` here means the \
         record could not be read at all, which the old `is_empty()` form passed on"
    );
    assert_eq!(
        window_is_armed(),
        Some(true),
        "…and the probe must be armed"
    );
}

/// The `entry_id` recorded on the anchor for `action` (`""` when unprobed).
fn anchor_entry_id(action: &str) -> &'static str {
    refinements()
        .find(|r| r.action == action)
        .unwrap_or_else(|| panic!("no anchor registered for action {action}"))
        .entry_id
}

#[refines(machine = "Shapes", action = "EarlyReturn", project = "test::project")]
fn early_return(n: u32) -> u32 {
    if n == 0 {
        return 99;
    }
    n * 2
}

#[refines(machine = "Shapes", action = "Generic", project = "test::project")]
fn generic_identity<T: Clone>(t: &T) -> T {
    t.clone()
}

#[refines(machine = "Shapes", action = "Recursive", project = "test::project")]
fn recursive_sum(n: u64) -> u64 {
    if n == 0 { 0 } else { n + recursive_sum(n - 1) }
}

/// `const fn` is the one shape that CANNOT be probed (a const body may not call a
/// non-const fn). It must still compile, still be usable in const position, and
/// report itself as unprobed via an empty `entry_id` — which the gate treats as a
/// failure rather than a pass, so this is not an escape hatch.
#[refines(machine = "Shapes", action = "ConstFn", project = "test::project")]
const fn const_double(n: u32) -> u32 {
    n * 2
}

const CONST_EVALUATED: u32 = const_double(21);

/// Inner attributes on the block must survive the prepend.
#[refines(machine = "Shapes", action = "InnerAttr", project = "test::project")]
fn inner_attr_body() -> u32 {
    #![allow(clippy::let_and_return)]
    let v = 7;
    v
}

struct ByValue {
    n: u32,
}

impl ByValue {
    /// `self` BY VALUE, and a tail expression that moves out of it.
    #[refines(machine = "Shapes", action = "SelfByValue", project = "test::project")]
    fn into_n(self) -> u32 {
        self.n
    }

    /// FOUR anchors on one method — the `Terminal::post_process` shape. Attribute
    /// macros expand outside-in, so each expansion prepends its own probe to the
    /// body the next one sees, and all four ids must be recorded by one call.
    #[refines(machine = "Shapes", action = "Multi1", project = "test::project")]
    #[refines(machine = "Shapes", action = "Multi2", project = "test::project")]
    #[refines(machine = "Shapes", action = "Multi3", project = "test::project")]
    #[refines(machine = "Shapes", action = "Multi4", project = "test::project")]
    fn multi(&mut self) -> u32 {
        self.n += 1;
        self.n
    }
}

#[test]
fn probe_preserves_behaviour_of_every_awkward_fn_shape() {
    assert_eq!(early_return(0), 99, "an early return still returns early");
    assert_eq!(early_return(3), 6, "the tail expression is still the value");
    assert_eq!(generic_identity(&"x".to_string()), "x");
    assert_eq!(recursive_sum(4), 10, "recursion is unaffected");
    assert_eq!(const_double(4), 8);
    assert_eq!(CONST_EVALUATED, 42, "the const fn is still const-evaluable");
    assert_eq!(inner_attr_body(), 7);
    assert_eq!(ByValue { n: 5 }.into_n(), 5, "`self` by value still moves");
    let mut b = ByValue { n: 1 };
    assert_eq!(b.multi(), 2);
    assert_eq!(b.multi(), 3, "four probes do not disturb `&mut self` state");
}

#[test]
fn probe_records_the_anchor_id_the_record_carries() {
    open_window();

    let id = anchor_entry_id("EarlyReturn");
    assert_eq!(id, "Shapes::EarlyReturn @ early_return");
    assert!(
        !entered_anchor_ids().contains(id),
        "an un-called fn must NOT be recorded — this is the whole point"
    );

    let _ = early_return(1);
    assert!(
        entered_anchor_ids().contains(id),
        "calling the annotated fn must record its anchor id"
    );
}

#[test]
fn every_anchor_of_a_multi_anchored_fn_is_recorded_by_one_call() {
    open_window();
    let ids: Vec<&'static str> = ["Multi1", "Multi2", "Multi3", "Multi4"]
        .iter()
        .map(|a| anchor_entry_id(a))
        .collect();
    for id in &ids {
        assert!(id.ends_with(" @ multi"), "unexpected id {id}");
        assert!(!entered_anchor_ids().contains(id));
    }
    let _ = ByValue { n: 0 }.multi();
    let entered = entered_anchor_ids();
    for id in &ids {
        assert!(
            entered.contains(id),
            "{id} must be recorded by the one call"
        );
    }
}

#[test]
fn a_const_fn_anchor_reports_itself_unprobed_rather_than_lying() {
    open_window();
    assert_eq!(
        anchor_entry_id("ConstFn"),
        "",
        "a const fn cannot host the probe, and the record must SAY so — a gate that \
         demands execution evidence fails on an empty entry_id"
    );
    let _ = const_double(1);
    assert!(
        entered_anchor_ids().is_empty(),
        "the const fn records nothing at all"
    );
}

/// THE NON-NESTING RULE, ENFORCED rather than documented.
///
/// `StepEvidence::step`'s exit disarms unconditionally, so a window opened INSIDE the
/// drive closure closes the outer one early: the outer step then snapshots whatever
/// the inner window recorded and is credited with it. That was a doc comment
/// ("Windows MUST NOT NEST") with nothing behind it; the arm bit is now re-read after
/// `drive` returns, so the fail-open is a panic.
#[test]
#[should_panic(expected = "was CLOSED while this step ran")]
fn a_nested_window_panics_instead_of_lending_the_outer_step_its_evidence() {
    let mut outer = aterm_spec::xref::StepEvidence::new("Shapes");
    outer.step("EarlyReturn", || {
        let mut inner = aterm_spec::xref::StepEvidence::new("Shapes");
        inner.step("Multi1", || {
            let _ = ByValue { n: 0 }.multi();
        });
    });
}
