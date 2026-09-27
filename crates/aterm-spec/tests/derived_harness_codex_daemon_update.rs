// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_codex_daemon_update_model, interp, verify};

/// The Codex daemon is moved onto the managed build only while nothing runs
/// in it — no thread's turn, a detached thread's included, and no background
/// terminal a finished turn left running — never back from a build the vendor
/// already moved it ahead to, never against the owner's word on a tab it
/// serves, and never under a client the sweep did not see. Tier-0 here; the
/// Tier-1 bind to the real `daemon_step` is in aterm-agent's
/// `harness::upgrade_codex` tests.
#[test]
fn the_daemon_moves_only_between_turns_and_never_backwards() {
    let model = harness_codex_daemon_update_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "harness codex daemon update");

    // A detached thread's turn holds the daemon where it is.
    let mut detached = model.init_state();
    assert!(model.fire("StartDetached", &mut detached));
    assert!(!model.action_enabled("Update", &detached));
    assert!(model.fire("EndDetached", &mut detached));
    assert!(model.fire("Update", &mut detached));

    // So does a background terminal, its thread idle.
    let mut terminal = model.init_state();
    assert!(model.fire("StartTerminal", &mut terminal));
    assert!(!model.action_enabled("Update", &terminal));
    assert!(model.fire("EndTerminal", &mut terminal));
    assert!(model.action_enabled("Update", &terminal));

    // And the owner's word, and a client nobody asked.
    for (hold, release) in [
        ("OwnerHolds", "OwnerReleases"),
        ("ClientUnseen", "ClientGone"),
    ] {
        let mut s = model.init_state();
        assert!(model.fire(hold, &mut s));
        assert!(!model.action_enabled("Update", &s), "{hold}");
        assert!(model.fire(release, &mut s));
        assert!(model.action_enabled("Update", &s), "{release}");
    }

    // A daemon the vendor moved ahead stays there.
    let mut ahead = model.init_state();
    assert!(model.fire("VendorAhead", &mut ahead));
    assert!(!model.action_enabled("Update", &ahead));

    // NEGATIVE CONTROL: the rule that asks only the tabs it can see.
    let buggy = interp::with_buggy(&model, 1);
    for (start, invariant) in [
        ("StartDetached", "NoRunningWorkInterrupted"),
        ("StartTerminal", "NoRunningWorkInterrupted"),
        ("VendorAhead", "NeverOntoAnOlderBuild"),
        ("OwnerHolds", "NeverAgainstTheOwnersWord"),
        ("ClientUnseen", "NeverPastAnUnseenClient"),
    ] {
        let mut cut = buggy.init_state();
        assert!(buggy.fire(start, &mut cut), "{start}");
        assert!(buggy.fire("Update", &mut cut), "{start}");
        assert!(!buggy.check_invariant(invariant, &cut), "{invariant}");
    }
}
