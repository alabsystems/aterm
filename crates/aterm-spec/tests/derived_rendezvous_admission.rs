// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateRendezvousAdmission`: the outgoing process grants only to a dialer
//! whose code is the successor's, commits to none LaunchServices disowns, turns no dialer away
//! for a code check `trustd` did not answer, and files a refusal by its cause; each rule has its
//! caught mutant.

/// Each mutant, alone, breaks the invariant it is named for, and only that one.
#[test]
fn each_mutant_breaks_its_own_invariant() {
    use aterm_spec::interp::with_buggy;
    const INVARIANTS: [&str; 4] = [
        "GrantOnlyToTheSuccessorsCode",
        "NoCommitToADisownedDialer",
        "AnUnansweredCheckTurnsNoOneAway",
        "ARefusalIsFiledByItsCause",
    ];
    let buggy = with_buggy(
        &aterm_spec::derive::native_update_rendezvous_admission_model(),
        1,
    );
    for (path, broken) in [
        (
            &["CopiedSecretOtherCode", "GrantOnSecretAlone"][..],
            "GrantOnlyToTheSuccessorsCode",
        ),
        (
            &["Grant", "LaunchServicesDisowns", "CommitOverDisowning"][..],
            "NoCommitToADisownedDialer",
        ),
        (
            &["TrustdSilent", "RefuseUnanswered"][..],
            "AnUnansweredCheckTurnsNoOneAway",
        ),
        (
            &["CopiedSecretOtherCode", "Refuse", "FileAsUnexplainedDeath"][..],
            "ARefusalIsFiledByItsCause",
        ),
    ] {
        let mut state = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
        }
        for invariant in INVARIANTS {
            assert_eq!(
                buggy.check_invariant(invariant, &state),
                invariant != broken,
                "{path:?} and {invariant}: {state:?}"
            );
        }
    }
}

/// The hold: a dialer whose check `trustd` did not answer is neither granted nor refused; it is
/// granted once the check is answered, and otherwise the deadline ends the wait, filed by that
/// cause.
#[test]
fn an_unanswered_check_holds_until_it_is_answered_or_the_deadline() {
    let model = aterm_spec::derive::native_update_rendezvous_admission_model();
    let mut held = model.init_state();
    assert!(model.fire("TrustdSilent", &mut held));
    assert!(!model.action_enabled("Grant", &held), "never a pass");
    assert!(!model.action_enabled("Refuse", &held), "never a refusal");
    let mut answered = held.clone();
    assert!(model.fire("TrustdAnswers", &mut answered));
    assert!(model.fire("Grant", &mut answered));
    assert!(model.fire("DeadlineWhileHeld", &mut held));
    assert!(model.fire("FileByCause", &mut held));
}
