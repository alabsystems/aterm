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
