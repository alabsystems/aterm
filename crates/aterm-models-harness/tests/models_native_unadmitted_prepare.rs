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
        // The agent's registration was the host's before it was recreated.
        (
            8,
            "ReentryOwnRegistration",
            &["Register", "Recreate", "Ask", "Approach", "Reenter"][..],
        ),
        // Another operation of this host's epoch is bound when the re-entry comes.
        (
            9,
            "ReentryUnbound",
            &[
                "Settle", "Recreate", "Register", "Bind", "Ask", "Approach", "Reenter",
            ][..],
        ),
        // The recreated host is asked to succeed the predecessor its previous process settled.
        (
            10,
            "NoRevival",
            &["Settle", "Recreate", "Ask", "Approach", "QualifyUpgrade"][..],
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

#[test]
fn a_recreated_host_is_reentered_from_its_own_registration_and_never_revives_a_predecessor() {
    let base = native_unadmitted_prepare_model();
    let mut state = base.init_state();
    for action in [
        "Settle", "Register", "Recreate", "Ask", "Approach", "Register", "Reenter", "Reserve",
        "Publish", "Record", "Start", "Complete",
    ] {
        assert!(base.fire(action, &mut state), "{action} from {state:?}");
    }
    for law in &base.invariants {
        assert!(base.check_invariant(law.name, &state), "{}", law.name);
    }
    // The settling host's own upgrade qualifies and publishes; an unqualified asked prepare
    // never publishes, whatever its deadline.
    let mut upgrade = base.init_state();
    for action in [
        "Settle",
        "Ask",
        "Approach",
        "QualifyUpgrade",
        "Reserve",
        "Publish",
    ] {
        assert!(base.fire(action, &mut upgrade), "{action} from {upgrade:?}");
    }
    // Once completed with nothing a later asked move succeeds (a close, an abort after its
    // signal), the upgrade leaves the table binding nothing, settling nothing it could revive.
    let mut ended = upgrade.clone();
    for action in ["Record", "Start", "Complete", "Unbind"] {
        assert!(base.fire(action, &mut ended), "{action} from {ended:?}");
    }
    assert_eq!((ended["settled"], ended["bound"]), (0, 0));
    for law in &base.invariants {
        assert!(base.check_invariant(law.name, &ended), "{}", law.name);
    }
    let mut unqualified = base.init_state();
    for action in ["Ask", "Approach", "Reserve"] {
        assert!(base.fire(action, &mut unqualified), "{action}");
    }
    assert!(!base.action_enabled("Publish", &unqualified));
    // A recreated host settles nothing, so the upgrade naming the old predecessor stays
    // unqualified until the host's own registration re-enters.
    let mut recreated = base.init_state();
    for action in ["Settle", "Recreate", "Ask", "Approach"] {
        assert!(base.fire(action, &mut recreated), "{action}");
    }
    assert!(!base.action_enabled("QualifyUpgrade", &recreated));
    assert!(!base.action_enabled("Reenter", &recreated));
    assert!(base.fire("Register", &mut recreated));
    assert!(base.action_enabled("Reenter", &recreated));
}
