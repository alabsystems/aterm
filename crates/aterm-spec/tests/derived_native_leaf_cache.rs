// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::native_leaf_cache_lifetime_model, interp, verify};

#[test]
fn native_leaf_cache_retires_hidden_rasters_on_every_route() {
    let model = native_leaf_cache_lifetime_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "visible native leaf cache ownership");
    assert!(verify::uncaught_invariants(&model).is_empty());

    let buggy = interp::with_buggy(&model, 1);
    for bind in ["BindTerminal", "BindFirst", "BindSecond"] {
        let mut state = buggy.init_state();
        for action in ["BindSplit", "Paint", bind] {
            assert!(buggy.fire(action, &mut state));
        }
        assert!(
            !buggy.check_invariant("OnlyVisibleLeavesRetainRasters", &state),
            "the historical {bind} route must expose retained hidden rasters"
        );
    }
}
