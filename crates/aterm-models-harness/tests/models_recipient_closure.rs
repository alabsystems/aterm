// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn recipient_closure_refuses_unsafe_recovery_controls() {
    let model = aterm_models_harness::recipient_closure_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    for (bug, law, path) in [
        (
            1,
            "NoDoubleExec",
            "Complete Prepare ReadJournal RecordF DeliverF Expire ReadClock Answer Fresh",
        ),
        (
            2,
            "NoDoubleExec",
            "Complete Prepare RecordR DeliverR Crash Expire ReadClock ReadJournal Probe Answer Fresh",
        ),
        (
            3,
            "NoDoubleExec",
            "Complete Prepare RecordF DeliverF ExitF Crash Expire ReadClock ReadJournal Probe Answer Fresh",
        ),
        (
            4,
            "NoDoubleExec",
            "Complete Prepare RecordF DeliverF Crash Expire UnknownBirth ReadClock ReadJournal Probe Answer Fresh",
        ),
        (
            5,
            "DeadlineOrFinal",
            "Complete Prepare ReadClock ReadJournal Answer",
        ),
        (
            6,
            "CompleteProof",
            "Prepare LegacyDeliver Expire ReadClock ReadJournal Probe Answer",
        ),
        (
            7,
            "LocalProof",
            "Complete Prepare Foreign ReadClock ReadJournal Answer",
        ),
        (
            8,
            "NoExecHeld",
            "Complete Prepare NoExec ReadClock ReadJournal Answer",
        ),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        for action in path.split_whitespace() {
            assert!(
                mutant.fire(action, &mut state),
                "{bug}: {action}: {state:?}"
            );
        }
        assert!(!model.check_invariant(law, &state), "{bug}: {law}");
    }
}
