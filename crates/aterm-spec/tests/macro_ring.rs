// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! `ty_model!` is pure sugar: the ring written through the macro derives the same TLA+, `.cfg`
//! and transition spec as the hand-built `ring_model()`.

use aterm_spec::derive::{Model, ring_model};
use aterm_spec::ty_model;

/// The bounded event-log ring, written in the light-annotation surface.
fn ring_via_macro() -> Model {
    ty_model! {
        Ring {
            const MaxSeq = 6;
            const Cap = 3;
            const Buggy = 0;
            var seq = 0;
            var lo = 1;
            action Push when (seq <= MaxSeq - 1) {
                seq = seq + 1;
                lo = if (seq + 1) - lo + 1 > Cap + Buggy { lo + 1 } else { lo };
            }
            invariant LenBounded: seq - lo + 1 <= Cap;
        }
    }
}

#[test]
fn macro_surface_equals_hand_built_model() {
    let macro_built = ring_via_macro();
    let hand_built = ring_model();
    assert_eq!(
        macro_built.to_tla(),
        hand_built.to_tla(),
        "ty_model! must derive byte-identical TLA+ to the hand-built ring_model()"
    );
    assert_eq!(
        macro_built.to_cfg(),
        hand_built.to_cfg(),
        "derived .cfg must match"
    );
    assert_eq!(
        macro_built.transition_spec(),
        hand_built.transition_spec(),
        "open-Init (conformance) form must match too"
    );
}

#[test]
fn macro_built_interpreter_runs() {
    let m = ring_via_macro();
    let mut st = m.init_state();
    while m.fire("Push", &mut st) {
        assert!(
            m.check_invariant("LenBounded", &st),
            "LenBounded holds at seq={}",
            st[&"seq"]
        );
    }
    assert_eq!(st[&"seq"], 6, "guard bounds seq at MaxSeq");
}
