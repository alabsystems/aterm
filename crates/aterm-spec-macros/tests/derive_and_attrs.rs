// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the self-contained surface of `aterm-spec-macros`:
//! the `SpecState` / `SpecAction` derives and the `refines` / `spec_invariant`
//! / `spec_unmodeled` attribute macros.
//!
//! Since TRUST_NATIVE_TLA Phase 0 the attribute macros are ANCHOR EMITTERS: they
//! leave the annotated item unchanged AND emit an `inventory` record referencing
//! `::aterm_spec::{inventory,xref}::…`. So this test now links `aterm-spec` (a
//! dev-dependency; the cycle is dev-only and Cargo-legal). The `ty_model!` macro
//! emits `::aterm_spec::derive::*` paths and is exercised from `aterm-spec`'s own
//! `tests/`.

use aterm_spec_macros::{SpecAction, SpecState, refines, spec_invariant, spec_unmodeled};

// ── SpecState: explicit name + tla_file attributes ───────────────────────────

#[derive(SpecState)]
#[spec_machine(name = "ring", tla_file = "Evict.tla")]
struct RingState {
    #[allow(dead_code)]
    seq: u64,
}

#[test]
fn spec_state_explicit_name_and_file() {
    assert_eq!(RingState::SPEC_MACHINE_NAME, "ring");
    assert_eq!(RingState::SPEC_TLA_FILE, "Evict.tla");
}

// ── SpecState: default machine name (strip trailing "Model", lowercase) ──────

#[derive(SpecState)]
struct KernelModel;

#[test]
fn spec_state_default_name_strips_model_suffix_and_lowercases() {
    // "KernelModel" -> strip "Model" -> "Kernel" -> lowercase -> "kernel"
    assert_eq!(KernelModel::SPEC_MACHINE_NAME, "kernel");
    // tla_file defaults to the empty string when not provided
    assert_eq!(KernelModel::SPEC_TLA_FILE, "");
}

// ── SpecState: a name with no "Model" suffix just lowercases ──────────────────

#[derive(SpecState)]
struct Cursor;

#[test]
fn spec_state_default_name_without_model_suffix() {
    assert_eq!(Cursor::SPEC_MACHINE_NAME, "cursor");
}

// ── SpecState: only one attribute key supplied (the other falls back) ────────

#[derive(SpecState)]
#[spec_machine(tla_file = "Subscribe.tla")]
struct PartialAttrModel;

#[test]
fn spec_state_partial_attr_falls_back_for_missing_key() {
    // name not given -> derived from type ("PartialAttrModel" -> "partialattr")
    assert_eq!(PartialAttrModel::SPEC_MACHINE_NAME, "partialattr");
    assert_eq!(PartialAttrModel::SPEC_TLA_FILE, "Subscribe.tla");
}

// ── SpecAction: variant names collected into SPEC_ACTIONS, in order ──────────

#[derive(SpecAction)]
#[allow(dead_code)]
enum RingAction {
    Push,
    Evict,
    Reset,
}

#[test]
fn spec_action_collects_variant_names_in_order() {
    assert_eq!(RingAction::SPEC_ACTIONS, ["Push", "Evict", "Reset"]);
    assert_eq!(RingAction::SPEC_ACTIONS.len(), 3);
}

// ── SpecAction: works on variants carrying data; uses only the variant ident ─

#[derive(SpecAction)]
#[allow(dead_code)]
enum DataAction {
    Grow(u32),
    Deliver { cursor: u64 },
    Idle,
}

#[test]
fn spec_action_ignores_variant_payloads() {
    assert_eq!(DataAction::SPEC_ACTIONS, ["Grow", "Deliver", "Idle"]);
}

// ── SpecAction: empty enum yields a zero-length action array ─────────────────

#[derive(SpecAction)]
enum NoAction {}

#[test]
fn spec_action_empty_enum_is_empty_array() {
    assert_eq!(NoAction::SPEC_ACTIONS.len(), 0);
    let _ = |x: NoAction| match x {};
}

// ── Attribute macros are pass-throughs: the annotated item still works ────────
// `refines` accepts `machine = "..", action = ".."`; the function it annotates
// must remain callable and unmodified.

#[refines(machine = "Ring", action = "Push")]
fn push_impl(n: u32) -> u32 {
    n + 1
}

#[test]
fn refines_attribute_preserves_item() {
    assert_eq!(push_impl(41), 42);
}

// `spec_invariant` accepts `id = ".."` and an optional `tla = ".."`.

#[spec_invariant(id = "LenBounded", tla = "seq - lo + 1 <= Cap")]
fn check_len(len: usize, cap: usize) -> bool {
    len <= cap
}

#[spec_invariant(id = "NoSilentLoss")]
fn no_loss() -> bool {
    true
}

#[test]
fn spec_invariant_attribute_preserves_item_with_and_without_optional_tla() {
    assert!(check_len(2, 3));
    assert!(!check_len(4, 3));
    assert!(no_loss());
}

// `spec_unmodeled` accepts `reason = ".."`.

#[spec_unmodeled(reason = "platform-specific fast path")]
fn platform_fast_path() -> &'static str {
    "fast"
}

#[test]
fn spec_unmodeled_attribute_preserves_item() {
    assert_eq!(platform_fast_path(), "fast");
}

// `refines` on an impl-block method (item position generality).

struct Engine;

impl Engine {
    #[refines(machine = "Cursor", action = "Deliver")]
    fn deliver(&self, seq: u64) -> u64 {
        seq
    }
}

#[test]
fn refines_on_impl_method_preserves_item() {
    assert_eq!(Engine.deliver(7), 7);
}

// ── EXECUTION EVIDENCE: the fn-entry probe `#[refines]` injects ──────────────
//
// The emitter no longer leaves the annotated fn untouched: it prepends
// `::aterm_spec::xref::note_entered("<machine>::<action> @ <fn>")` to the body and
// records that same id on the submitted `RefinementAnchor`. These tests pin the two
// properties that make that safe to do to ~190 anchors across the tree: the
// annotated function's BEHAVIOUR is unchanged for every awkward fn shape, and the
// probe actually fires (so a gate can tell a live seam from a stub).
//
// Thread-locality is what makes the assertions below exact: each `#[test]` runs on
// its own thread, so one test's entries are invisible to another even though the
// harness runs them in parallel.

use aterm_spec::xref::{
    disarm_entered_anchors, entered_anchor_count, entered_anchor_ids, refinements,
    reset_entered_anchors, window_is_armed,
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

#[refines(machine = "Shapes", action = "EarlyReturn")]
fn early_return(n: u32) -> u32 {
    if n == 0 {
        return 99;
    }
    n * 2
}

#[refines(machine = "Shapes", action = "Generic")]
fn generic_identity<T: Clone>(t: &T) -> T {
    t.clone()
}

#[refines(machine = "Shapes", action = "Recursive")]
fn recursive_sum(n: u64) -> u64 {
    if n == 0 { 0 } else { n + recursive_sum(n - 1) }
}

/// `const fn` is the one shape that CANNOT be probed (a const body may not call a
/// non-const fn). It must still compile, still be usable in const position, and
/// report itself as unprobed via an empty `entry_id` — which the gate treats as a
/// failure rather than a pass, so this is not an escape hatch.
#[refines(machine = "Shapes", action = "ConstFn")]
const fn const_double(n: u32) -> u32 {
    n * 2
}

const CONST_EVALUATED: u32 = const_double(21);

/// Inner attributes on the block must survive the prepend.
#[refines(machine = "Shapes", action = "InnerAttr")]
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
    #[refines(machine = "Shapes", action = "SelfByValue")]
    fn into_n(self) -> u32 {
        self.n
    }

    /// FOUR anchors on one method — the `Terminal::post_process` shape. Attribute
    /// macros expand outside-in, so each expansion prepends its own probe to the
    /// body the next one sees, and all four ids must be recorded by one call.
    #[refines(machine = "Shapes", action = "Multi1")]
    #[refines(machine = "Shapes", action = "Multi2")]
    #[refines(machine = "Shapes", action = "Multi3")]
    #[refines(machine = "Shapes", action = "Multi4")]
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

/// `entered_anchor_count` reports what `entered_anchor_ids` cannot: the difference
/// between an empty record and an unreadable one.
///
/// The readable half is what the assertions in `open_window` and in
/// `StepEvidence::step` now rest on. The unreadable half (`None`) is a destroyed TLS
/// slot or an outstanding borrow, neither of which a test can stage without unsound
/// tricks — what matters is that the SIGNAL exists at all, because
/// `entered_anchor_ids().is_empty()` collapsed both cases to `true`.
#[test]
fn the_record_reports_its_size_rather_than_only_its_contents() {
    open_window();
    assert_eq!(entered_anchor_count(), Some(0));
    let _ = early_return(1);
    assert_eq!(
        entered_anchor_count(),
        Some(entered_anchor_ids().len()),
        "the count and the contents must agree while the record is readable"
    );
    assert!(entered_anchor_count().is_some_and(|n| n > 0));
    disarm_entered_anchors();
    assert_eq!(
        window_is_armed(),
        Some(false),
        "closing the window must be observable — `StepEvidence::step` reads this to \
         detect a window that did not survive its own drive closure"
    );
}

/// THE COST CONTRACT, asserted behaviourally rather than promised in a comment.
///
/// The probe is injected into ~190 bodies tree-wide, including `Terminal::write_char`
/// — the per-character path for every non-ASCII, styled, insert-mode and VT52 write —
/// and including bodies inside `aterm-gui`'s `frame_latency` criterion benchmark's
/// measured region. So an UN-ARMED probe must do nothing at all beyond one
/// thread-local `bool` load: no `RefCell` borrow, no `BTreeSet` insert, no string
/// comparisons down a tree that grows with every anchor a conformance has entered.
///
/// "Does nothing" is observable exactly as "records nothing", which is what this
/// asserts. Only `reset_entered_anchors` arms, and `disarm_entered_anchors` closes the
/// window again.
#[test]
fn an_unarmed_probe_records_nothing() {
    let id = anchor_entry_id("EarlyReturn");

    // Close any window explicitly rather than assuming this thread never opened one:
    // `--test-threads=1` runs every test in this file on ONE thread, and the record is
    // thread-local, so "no test has armed yet" is not a property this test may assume.
    disarm_entered_anchors();
    let baseline = entered_anchor_ids();
    let _ = early_return(1);
    assert_eq!(
        entered_anchor_ids(),
        baseline,
        "a probe outside any evidence window must record NOTHING — it is on a \
         per-character hot path and inside a benchmark's measured region"
    );

    open_window();
    let _ = early_return(1);
    assert!(
        entered_anchor_ids().contains(id),
        "…and inside a window it must record"
    );

    disarm_entered_anchors();
    let before = entered_anchor_ids();
    let _ = early_return(1);
    let _ = ByValue { n: 0 }.multi();
    assert_eq!(
        entered_anchor_ids(),
        before,
        "closing the window must stop the recording, without discarding what the \
         window already saw"
    );
}
