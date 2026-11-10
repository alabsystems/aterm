// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn reservation_ownership_laws_have_falsifying_controls() {
    let model = aterm_models_harness::recipient_reservation_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    for (bug, law, path) in [
        (1, "LegacyOwns", "Legacy Reserve"),
        (2, "ReservationOwns", "Reserve Legacy"),
        (3, "ReservationOwns", "Reserve Lose"),
        (4, "Capacity", "Tight Reserve"),
        (5, "Domain", "WrongDomain Reserve"),
        (6, "Custody", "Reserve Lose Reserve"),
        (7, "AckOwns", "Reserve"),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        for action in path.split_whitespace() {
            assert!(mutant.fire(action, &mut state), "{bug}: {action}");
        }
        assert!(!model.check_invariant(law, &state), "{bug}: {law}");
    }
}
