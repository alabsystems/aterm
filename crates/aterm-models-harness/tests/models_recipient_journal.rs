// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn recipient_journal_laws_have_falsifying_controls() {
    let model = aterm_models_harness::recipient_journal_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    for (bug, law, path) in [
        (1, "ForwardBeforeRollback", "Prepare Rollback Forward"),
        (2, "Counted", "Prepare Rollback Rollback"),
        (
            2,
            "Finite",
            "Prepare Rollback Rollback Rollback Rollback Rollback",
        ),
        (3, "NoExecNeverGranted", "Prepare Forward NoExec"),
        (4, "NoDowngrade", "Prepare LegacyWrite"),
        (5, "Capacity", "Prepare Forward Other Rollback"),
        (6, "LegacyNeverComplete", "Legacy Prepare"),
        (7, "ReadOnlyComplete", "Legacy Read"),
        (8, "Terminal", "Prepare Seal Forward"),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        for action in path.split_whitespace() {
            assert!(mutant.fire(action, &mut state), "{bug}: {action}");
        }
        assert!(!model.check_invariant(law, &state), "{bug}: {law}");
    }
}
