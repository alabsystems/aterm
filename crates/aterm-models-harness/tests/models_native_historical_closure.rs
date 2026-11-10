// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_harness::native_historical_closure_model;

#[test]
fn each_historical_closure_law_has_a_falsifying_control() {
    let base = native_historical_closure_model();
    aterm_spec::interp::prove_and_catch(&base);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&base));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    // The journal read first: a grant lands between the two reads.
    let read_first: &[&str] = &[
        "Record",
        "ReadJournal",
        "May",
        "Deliver",
        "Expire",
        "ReadClock",
        "AnswerLapsed",
        "Fresh",
    ];
    let may_answered: &[&str] = &[
        "Record",
        "May",
        "Deliver",
        "Expire",
        "ReadClock",
        "ReadJournal",
        "AnswerLapsed",
        "Fresh",
    ];
    let clock_ignored: &[&str] = &[
        "Record",
        "ReadClock",
        "ReadJournal",
        "AnswerLapsed",
        "May",
        "Deliver",
        "Fresh",
    ];
    let unstamped: &[&str] = &[
        "Legacy",
        "Deliver",
        "Expire",
        "ReadClock",
        "ReadJournal",
        "AnswerLapsed",
        "Fresh",
    ];
    let set_aside: &[&str] = &[
        "StampCustody",
        "Record",
        "May",
        "Deliver",
        "Crash",
        "Move",
        "Expire",
        "ReadClock",
        "ReadJournal",
        "AnswerLapsed",
        "Fresh",
    ];
    let same_boot: &[&str] = &[
        "StampBoot",
        "Record",
        "May",
        "Deliver",
        "ReadClock",
        "ReadJournal",
        "AnswerBoot",
        "Fresh",
    ];
    let rollback_alive: &[&str] = &[
        "Record",
        "May",
        "Deliver",
        "ChildExits",
        "DeliverRollback",
        "Crash",
        "Expire",
        "ReadClock",
        "ReadJournal",
        "AnswerLapsed",
        "Fresh",
    ];
    for (bug, law, path) in [
        (1, "LapsedNeverGranted", read_first),
        (1, "NoDoubleExec", read_first),
        (2, "LapsedNeverGranted", may_answered),
        (2, "NoDoubleExec", may_answered),
        (3, "LapsedNeverGranted", clock_ignored),
        (3, "NoDoubleExec", clock_ignored),
        (4, "LapsedNeverGranted", unstamped),
        (4, "NoDoubleExec", unstamped),
        (5, "LapsedNeverGranted", set_aside),
        (5, "NoDoubleExec", set_aside),
        (
            5,
            "NoRevivalOverNoExec",
            &[
                "StampCustody",
                "Record",
                "NoExec",
                "Crash",
                "Move",
                "Expire",
                "ReadClock",
                "ReadJournal",
                "AnswerLapsed",
            ][..],
        ),
        (6, "BootOnlyAfterReboot", same_boot),
        (6, "NoDoubleExec", same_boot),
        (8, "LapsedNeverGranted", rollback_alive),
        (8, "NoDoubleExec", rollback_alive),
        (
            7,
            "NoRevivalOverNoExec",
            &[
                "StampBoot",
                "StampCustody",
                "Record",
                "NoExec",
                "Move",
                "Reboot",
                "ReadClock",
                "ReadJournal",
                "AnswerBoot",
            ][..],
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
        assert!(refused, "the correct reader refuses mutant {bug}'s answer");
    }
}
