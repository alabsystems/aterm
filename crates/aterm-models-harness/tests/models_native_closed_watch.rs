// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_harness::native_closed_watch_model;

#[test]
fn each_closed_watch_law_has_a_falsifying_control() {
    let base = native_closed_watch_model();
    aterm_spec::interp::prove_and_catch(&base);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&base));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    // A grantee that runs unregistered in a shell that gave up its tab: the watch holds.
    let unregistered: &[&str] = &["Launch", "Close"];
    let adopted: &[&str] = &["Launch", "Register", "Close", "Adopt"];
    for (bug, law, path) in [
        (
            1,
            "NoStartBesideTheGrantee",
            [unregistered, &["Clear", "Start"][..]].concat(),
        ),
        (
            1,
            "SignalsOnlyItsSubject",
            [unregistered, &["Clear", "SignalOther"][..]].concat(),
        ),
        (
            2,
            "NoStartBesideTheGrantee",
            [unregistered, &["Start"][..]].concat(),
        ),
        (
            3,
            "NoStartBesideTheGrantee",
            [adopted, &["Start"][..]].concat(),
        ),
        (
            4,
            "SignalsOnlyItsSubject",
            [unregistered, &["SignalOther"][..]].concat(),
        ),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&base, bug);
        let mut bad = mutant.init_state();
        let mut caught = false;
        for action in &path {
            assert!(mutant.fire(action, &mut bad), "mutant {bug}: {action}");
            caught |= !base.check_invariant(law, &bad);
        }
        assert!(caught, "mutant {bug} must violate {law}");
        let mut good = base.init_state();
        let refused = path.iter().any(|action| !base.fire(action, &mut good));
        assert!(refused, "the correct watch refuses mutant {bug}'s step");
    }
}

/// The watch ends: past the close, whatever the grantee, its shell and the shell's other child
/// do, a kept watch can still be adopted or cleared once the world settles (the grantee exits,
/// or registers and its shell gives up the tab or goes; an agent the person started in the shell
/// exits). A child of the shell that runs no image of the restart's never keeps it.
#[test]
fn every_kept_watch_can_end() {
    let base = native_closed_watch_model();
    let mut checked = 0;
    let mut beside_a_helper = 0;
    for state in aterm_spec::interp::reachable_states(&base) {
        if state["watch"] != 1 {
            continue;
        }
        checked += 1;
        let ends = |state: &aterm_spec::interp::State| {
            base.action_enabled("Adopt", state) || base.action_enabled("Clear", state)
        };
        if state["other"] == 1 && state["grantee"] != 1 {
            beside_a_helper += 1;
            assert!(ends(&state), "a helper keeps the watch: {state:?}");
        }
        if ends(&state) {
            continue;
        }
        // Only a running grantee, or a child the watch cannot tell from it, holds the watch, and
        // their exits always clear it.
        assert!(state["grantee"] == 1 || state["other"] == 2, "{state:?}");
        let mut exited = state.clone();
        if exited["grantee"] == 1 {
            assert!(base.fire("Exit", &mut exited), "{state:?}");
        }
        if exited["other"] == 2 {
            assert!(base.fire("OtherExits", &mut exited), "{state:?}");
        }
        assert!(ends(&exited), "{state:?}");
    }
    assert!(checked > 0, "some watch is kept");
    assert!(beside_a_helper > 0, "some watch is kept beside a helper");
}
