// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_session_index_retry_model, interp, verify};

#[test]
fn same_build_retries_wait_longer_but_a_newer_build_bypasses_them() {
    let model = atpkg_session_index_retry_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg session index retry");

    let mut s = model.init_state();
    assert!(model.fire("First", &mut s));
    assert!(model.fire("EarlySame", &mut s));
    assert_eq!(s["passes"], 1, "same build before five minutes is held");
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("RetryFirst", &mut s));
    assert_eq!(s["passes"], 2, "five minutes admits one retry");
    for _ in 0..2 {
        assert!(model.fire("Tick", &mut s));
        assert!(model.fire("EarlySame", &mut s));
        assert_eq!(s["passes"], 2);
    }
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("RetrySecond", &mut s));
    assert_eq!(s["passes"], 3, "fifteen minutes admits the next");
    assert!(model.fire("NewerHint", &mut s));
    assert_eq!((s["newer"], s["passes"], s["age"]), (1, 1, 0));

    let buggy = interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    assert!(buggy.fire("First", &mut old));
    assert!(buggy.fire("EarlySame", &mut old));
    assert!(!buggy.check_invariant("NoPrematureSameBuildPass", &old));
}
