// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn custody_enrollment_and_binding_laws_have_falsifying_controls() {
    let model = aterm_models_harness::recipient_custody_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    for (bug, law, path) in [
        (1, "EnrollSound", "Enroll LoseJournal Enroll"),
        (2, "BindSound", "Enroll Reserve LoseEnrollment Bind"),
        (3, "ReservationRequired", "Enroll Bind"),
        (4, "IdentityRequired", "Enroll Reserve WrongStamp Bind"),
        (
            5,
            "GenerationRequired",
            "Enroll Reserve Bind ReplaceReservation Bind",
        ),
        (6, "BindingProgress", "Enroll Reserve Bind"),
        (7, "EnrollmentProgress", "Pending Enroll"),
        (8, "BindingRecorded", "Enroll Reserve Bind"),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        for action in path.split_whitespace() {
            assert!(mutant.fire(action, &mut state), "{bug}: {action}");
        }
        assert!(!model.check_invariant(law, &state), "{bug}: {law}");
    }
}
