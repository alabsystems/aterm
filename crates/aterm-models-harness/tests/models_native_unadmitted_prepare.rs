// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_harness::native_unadmitted_prepare_model;

#[test]
fn each_unadmitted_answer_law_has_a_falsifying_control() {
    let base = native_unadmitted_prepare_model();
    aterm_spec::interp::prove_and_catch(&base);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&base));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    let admitted_then_reclaimed: &[&str] = &[
        "Approach", "Reserve", "Publish", "Record", "Start", "Complete", "Expire", "Reclaim",
        "Answer",
    ];
    for (bug, law, path) in [
        (
            1,
            "NeverRecorded",
            &[
                "Approach", "Expire", "Answer", "Reserve", "Publish", "Record",
            ][..],
        ),
        (2, "OnlyExpired", &["Approach", "Answer"][..]),
        (
            2,
            "NeverAdmitted",
            &[
                "Approach", "Answer", "Reserve", "Publish", "Record", "Start",
            ][..],
        ),
        (3, "OnlyExpired", &["Answer"][..]),
        (
            4,
            "NothingInMemory",
            &["Approach", "Reserve", "Publish", "Expire", "Answer"][..],
        ),
        (5, "NeverAdmitted", admitted_then_reclaimed),
        (5, "NeverRecorded", admitted_then_reclaimed),
        (
            6,
            "NeverAdmitted",
            &[
                "OwnImage", "Approach", "Reserve", "Publish", "Start", "Complete", "Expire",
                "Reclaim", "Answer",
            ][..],
        ),
        (
            7,
            "SupportedShape",
            &["UnsupportedAsk", "Approach", "Expire", "Answer"][..],
        ),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&base, bug);
        let mut bad = mutant.init_state();
        let mut caught = false;
        for action in path {
            assert!(mutant.fire(action, &mut bad), "mutant {bug}: {action}");
            caught |= !base.check_invariant(law, &bad);
        }
        assert!(caught, "mutant {bug} must violate {law}");
        let mut good = base.init_state();
        let refused = path.iter().any(|action| !base.fire(action, &mut good));
        assert!(refused, "the correct host refuses mutant {bug}'s answer");
    }
}
