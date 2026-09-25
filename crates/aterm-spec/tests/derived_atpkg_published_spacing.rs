// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_published_spacing_model, interp, verify};

#[test]
fn a_newer_index_escapes_spacing_but_same_failed_index_and_unknowns_wait() {
    let model = atpkg_published_spacing_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg published index spacing");

    for ending in ["EndOldOk", "EndOldFailed"] {
        let mut state = model.init_state();
        for action in [ending, "Publish45", "DecideGui", "DecideQueued"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert_eq!((state["gui"], state["queued"]), (1, 1));
    }
    for ending in ["EndSameFailed", "EndUnknown"] {
        let mut state = model.init_state();
        for action in [ending, "Publish45", "DecideGui", "DecideQueued"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert_eq!((state["gui"], state["queued"]), (2, 2));
    }
    for blocker in ["StaleWitness", "LiveHolder"] {
        let mut state = model.init_state();
        assert!(model.fire("EndOldOk", &mut state));
        assert!(model.fire(blocker, &mut state), "{blocker}");
        for action in ["Publish45", "DecideGui", "DecideQueued"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert_eq!((state["gui"], state["queued"]), (2, 2));
    }

    let old = interp::with_buggy(&model, 1);
    let mut lost = old.init_state();
    for action in ["EndOldOk", "Publish45", "DecideGui"] {
        assert!(old.fire(action, &mut lost), "{action}");
    }
    assert!(!old.check_invariant("FreshHigherRuns", &lost));

    // The same inverted reader verdict must also expose every unsafe run,
    // including both GUI and queued-child decisions. These are independent
    // design claims, not state-space bounds.
    for (actions, invariant) in [
        (
            &["EndSameFailed", "Publish45", "DecideQueued"][..],
            "SameTargetWaits",
        ),
        (
            &["EndUnknown", "Publish45", "DecideGui"][..],
            "UnknownTargetWaits",
        ),
        (
            &["EndOldOk", "LiveHolder", "Publish45", "DecideQueued"][..],
            "LiveHolderWaits",
        ),
        (
            &["EndOldOk", "StaleWitness", "Publish45", "DecideQueued"][..],
            "StaleWitnessWaits",
        ),
    ] {
        let mut unsafe_run = old.init_state();
        for action in actions {
            assert!(old.fire(action, &mut unsafe_run), "{action}");
        }
        assert!(
            !old.check_invariant(invariant, &unsafe_run),
            "{invariant} was not falsified by the inverted reader"
        );
    }

    let unsafe_bypass = interp::with_buggy(&model, 2);
    let mut repeated = unsafe_bypass.init_state();
    for action in ["EndSameFailed", "Publish45", "DecideQueued"] {
        assert!(unsafe_bypass.fire(action, &mut repeated), "{action}");
    }
    assert!(!unsafe_bypass.check_invariant("SameTargetWaits", &repeated));
}
