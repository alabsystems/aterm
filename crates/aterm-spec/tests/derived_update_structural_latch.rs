// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::native_update_structural_latch_model, interp, verify};

/// A STRUCTURAL convergence no longer strands every later release (gap 14,
/// 2026-09-26; round three). The latch on build N gets ONE re-sample after a
/// day — never the launch its boot trial reverts on, which is held and said
/// instead, and never PROMISED past the first count that rules it out (gap 14
/// review) — and a verified newer release takes N's place at the install path,
/// whatever the trial, without N being launched for it — and a retire that is
/// REFUSED leaves that release unspent, offered again at its retry deadline
/// (round three review). Build N stays covered whatever the digest. The
/// negative controls replay gap 14's own `Decide`, the refusal that spent the
/// release, the strand before gap 14 and its trial-blind fix, each in its own
/// runs. Tier-0 here; the Tier-1 bind to the real
/// `structural_latch`, `spend_physical_failure_budget` and
/// `AutoApplyManualOnly::covers` is aterm-gui's `native_updater_conformance.rs`.
#[test]
fn a_structural_latch_is_resampled_once_and_yields_to_a_newer_release() {
    let model = native_update_structural_latch_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "native update structural latch");
    assert!(
        verify::uncaught_invariants(&model).is_empty(),
        "every invariant carries its own mutant: {:?}",
        verify::uncaught_invariants(&model)
    );

    // A newer release supersedes N, room or none: the retire starts at the
    // look, and when it lands the latch goes with N's bytes. N is never
    // launched for it, and nothing is left to attempt.
    for trial in ["TrialHasRoom", "TrialIsSpent"] {
        let mut newer = model.init_state();
        assert!(model.fire(trial, &mut newer));
        assert!(model.fire("NewerArrives", &mut newer));
        assert!(model.fire("Decide", &mut newer));
        assert_eq!(
            (newer["latched"], newer["superseding"], newer["newer_spent"]),
            (1, 1, 1),
            "{trial}: the retire is under way and the latch holds meanwhile"
        );
        assert!(model.fire("Supersede", &mut newer));
        assert_eq!(
            (newer["latched"], newer["superseded"], newer["booted_old"]),
            (0, 1, 0),
            "{trial}: superseded, and N never booted"
        );
        assert!(
            !model.action_enabled("AttemptFails", &newer),
            "{trial}: no attempt at N follows a supersede"
        );
        assert!(!model.action_enabled("DayPasses", &newer));
    }

    // A refused retire leaves the latch, said, and does NOT spend the release
    // (round three review): a refusal can be a moment — a verification that ran
    // out of its budget, the apply lock held too long — so the release is
    // offered again once its retry deadline passes. Until then it still
    // outranks the day: N is not launched while a newer build is on its way.
    for trial in ["TrialHasRoom", "TrialIsSpent"] {
        let mut refused = model.init_state();
        assert!(model.fire(trial, &mut refused));
        assert!(model.fire("NewerArrives", &mut refused));
        assert!(model.fire("Decide", &mut refused));
        assert!(model.fire("SupersedeRefused", &mut refused));
        assert_eq!(
            (
                refused["latched"],
                refused["said"],
                refused["newer_spent"],
                refused["backoff"]
            ),
            (1, 1, 0, 1),
            "{trial}: refused, said, unspent, waiting out its retry deadline"
        );
        assert!(model.fire("Decide", &mut refused));
        assert_eq!(
            (refused["latched"], refused["superseding"]),
            (1, 0),
            "{trial}: not offered again before its retry deadline"
        );
        assert!(model.fire("DayPasses", &mut refused));
        assert!(model.fire("Decide", &mut refused));
        assert_eq!(
            (refused["latched"], refused["booted_old"]),
            (1, 0),
            "{trial}: the waiting release outranks the day"
        );
        assert!(model.fire("RetryDue", &mut refused));
        assert!(model.fire("Decide", &mut refused));
        assert_eq!(
            (
                refused["latched"],
                refused["superseding"],
                refused["newer_spent"]
            ),
            (1, 1, 1),
            "{trial}: at its deadline the release is offered again"
        );
        assert!(model.fire("Supersede", &mut refused));
        assert_eq!(
            (
                refused["latched"],
                refused["superseded"],
                refused["booted_old"]
            ),
            (0, 1, 0),
            "{trial}: and the retry lands, N never booted"
        );
    }

    // The day with room and no newer release: the one re-sample, then never
    // again.
    let mut room = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut room));
    assert!(model.fire("Decide", &mut room));
    assert!(model.fire("DayPasses", &mut room));
    assert!(model.fire("Decide", &mut room));
    assert_eq!((room["latched"], room["owed"], room["spent"]), (0, 0, 1));
    assert!(model.fire("AttemptFails", &mut room));
    assert_eq!(room["owed"], 0, "a failed re-sample owes no second one");
    assert!(!model.action_enabled("DayPasses", &room));

    // Both at once: the newer release outranks the day — no launch of N while a
    // newer build waits to take its place.
    let mut both = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut both));
    assert!(model.fire("DayPasses", &mut both));
    assert!(model.fire("NewerArrives", &mut both));
    assert!(model.fire("Decide", &mut both));
    assert_eq!(
        (both["latched"], both["superseding"], both["owed"]),
        (1, 1, 1)
    );

    // No room, found by the first count: the promised re-sample is withdrawn
    // at once, and said.
    let mut first = model.init_state();
    assert!(model.fire("TrialIsSpent", &mut first));
    assert!(model.fire("Decide", &mut first));
    assert_eq!(
        (first["latched"], first["owed"], first["said"]),
        (1, 0, 1),
        "no retry promised past the trial"
    );
    // With room the first count changes nothing.
    let mut roomy = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut roomy));
    assert!(model.fire("Decide", &mut roomy));
    assert_eq!((roomy["latched"], roomy["owed"], roomy["said"]), (1, 1, 0));
    // No room: the day finds nothing left to spend.
    let mut spent = first.clone();
    assert!(model.fire("DayPasses", &mut spent));
    assert!(model.fire("Decide", &mut spent));
    assert_eq!(spent["latched"], 1, "the day finds nothing left to spend");

    // Build N stays latched whatever the digest.
    let mut same = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut same));
    assert!(model.fire("ArmSameBuild", &mut same));
    assert_eq!((same["latched"], same["escaped"]), (1, 0));

    // A count that cannot be read (round six, finding 55): the day's due
    // looks each ask for an observation and count, and at the bound the
    // re-sample is withdrawn and the hold said — with room or without, since
    // `room` is then no reading. A reading that comes back clears the count.
    for trial in ["TrialHasRoom", "TrialIsSpent"] {
        let mut unread = model.init_state();
        assert!(model.fire(trial, &mut unread));
        assert!(model.fire("Decide", &mut unread));
        assert!(model.fire("TrialUnreadable", &mut unread));
        assert!(model.fire("Decide", &mut unread));
        if trial == "TrialIsSpent" {
            assert_eq!(unread["owed"], 0, "the first count already withdrew it");
            continue;
        }
        assert_eq!(
            (unread["looks"], unread["owed"]),
            (0, 1),
            "a look before the day counts nothing"
        );
        assert!(model.fire("DayPasses", &mut unread));
        let bound = unread_looks(&model);
        for look in 1..=bound {
            assert!(model.fire("Decide", &mut unread));
            assert_eq!(
                (unread["looks"], unread["pending"], unread["owed"]),
                (look, 1, 1),
                "look {look}: counted, and another observation asked for"
            );
        }
        let mut read_again = unread.clone();
        assert!(model.fire("Decide", &mut unread));
        assert_eq!(
            (
                unread["latched"],
                unread["owed"],
                unread["spent"],
                unread["said"],
                unread["looks"],
                unread["pending"]
            ),
            (1, 0, 1, 1, 0, 0),
            "at the bound: held, nothing promised, said"
        );
        assert!(model.fire("TrialReadAgain", &mut read_again));
        assert!(model.fire("Decide", &mut read_again));
        assert_eq!(
            (
                read_again["looks"],
                read_again["latched"],
                read_again["owed"]
            ),
            (0, 0, 0),
            "a reading with room releases the re-sample and clears the count"
        );
    }

    // NEGATIVE CONTROL, one arc per mutant.
    let buggy = interp::with_buggy(&model, 1);
    // GAP 14'S OWN `Decide` (0.94.0), looking at the newer release before the
    // day. With room: the latch opens for one more attempt at N, launched while
    // the newer release waits behind it.
    let mut retried = buggy.init_state();
    assert!(buggy.fire("TrialHasRoom", &mut retried));
    assert!(buggy.fire("NewerArrives", &mut retried));
    assert!(buggy.fire("Decide", &mut retried));
    assert_eq!(
        (
            retried["latched"],
            retried["superseding"],
            retried["newer_spent"]
        ),
        (0, 0, 1),
        "gap 14 with room: released for an attempt at N, no retire"
    );
    assert!(!buggy.check_invariant("NeverBootsTheOlderActivation", &retried));
    // …without room: held, said, and the release spent — it waits for the
    // Version menu.
    let mut held = buggy.init_state();
    assert!(buggy.fire("TrialIsSpent", &mut held));
    assert!(buggy.fire("NewerArrives", &mut held));
    assert!(buggy.fire("Decide", &mut held));
    assert_eq!(
        (
            held["latched"],
            held["superseding"],
            held["newer_spent"],
            held["said"]
        ),
        (1, 0, 1, 1),
        "gap 14 without room: held and said, the release spent"
    );
    assert!(!buggy.check_invariant("ANewerReleaseClearsTheLatch", &held));
    // THE REFUSAL THAT SPENT THE RELEASE (round three as first shipped), looking
    // at the newer release after the day: the look starts the retire as the
    // healthy lane does, and the refusal keeps the release spent, with no retry.
    let mut spent = buggy.init_state();
    assert!(buggy.fire("TrialHasRoom", &mut spent));
    assert!(buggy.fire("DayPasses", &mut spent));
    assert!(buggy.fire("NewerArrives", &mut spent));
    assert!(buggy.fire("Decide", &mut spent));
    assert_eq!(spent["superseding"], 1, "the look is the healthy one");
    let healthy_refusal = model.successors("SupersedeRefused", &spent);
    assert!(buggy.fire("SupersedeRefused", &mut spent));
    assert_ne!(
        healthy_refusal,
        vec![spent.clone()],
        "the healthy refusal unspends and schedules a retry"
    );
    assert_eq!((spent["newer_spent"], spent["backoff"]), (1, 0));
    assert!(buggy.fire("Decide", &mut spent));
    assert_eq!(
        (spent["latched"], spent["superseding"]),
        (1, 0),
        "the release is never offered again"
    );
    assert!(!buggy.check_invariant("ANewerReleaseClearsTheLatch", &spent));
    // THE STRAND BEFORE GAP 14: with room and no newer release, the day changes
    // nothing.
    let mut strand = buggy.init_state();
    assert!(buggy.fire("TrialHasRoom", &mut strand));
    assert!(buggy.fire("DayPasses", &mut strand));
    assert!(buggy.fire("Decide", &mut strand));
    assert_eq!(
        (strand["latched"], strand["owed"]),
        (1, 1),
        "the strand holds"
    );
    assert!(!buggy.check_invariant("ResampleAfterADay", &strand));
    // The trial-blind re-sample: it spends the reverting launch, silently.
    let mut blind = buggy.init_state();
    assert!(buggy.fire("TrialIsSpent", &mut blind));
    assert!(buggy.fire("DayPasses", &mut blind));
    assert!(buggy.fire("Decide", &mut blind));
    assert!(!buggy.check_invariant("NeverSpendsTheRevertingLaunch", &blind));
    assert!(!buggy.check_invariant("ATrialHoldIsSaid", &blind));
    // The verdict-less convergence: the failed re-sample owes another.
    assert!(buggy.fire("AttemptFails", &mut blind));
    assert!(!buggy.check_invariant("ResampleAtMostOnce", &blind));
    // The notice that goes on promising a retry the trial cannot afford.
    let mut promise = buggy.init_state();
    assert!(buggy.fire("TrialIsSpent", &mut promise));
    assert!(buggy.fire("Decide", &mut promise));
    assert_eq!(promise["owed"], 1, "the promise stands");
    assert!(!buggy.check_invariant("NoRetryPromisedPastTheTrial", &promise));
    // The digest-keyed latch: a re-publish of N escapes, for no reason.
    let mut escaped = buggy.init_state();
    assert!(buggy.fire("TrialHasRoom", &mut escaped));
    assert!(buggy.fire("ArmSameBuild", &mut escaped));
    assert!(!buggy.check_invariant("SameBuildStaysLatched", &escaped));
    assert!(!buggy.check_invariant("ReleasedOnlyByAnEvent", &escaped));
    // THE PROMISE KEPT FOREVER: at the bound the unreadable count is
    // forgotten, the re-sample still owed, and nothing asks again.
    let mut forever = buggy.init_state();
    assert!(buggy.fire("TrialHasRoom", &mut forever));
    assert!(buggy.fire("Decide", &mut forever));
    assert!(buggy.fire("TrialUnreadable", &mut forever));
    assert!(buggy.fire("DayPasses", &mut forever));
    for _ in 0..=unread_looks(&buggy) {
        assert!(buggy.fire("Decide", &mut forever));
    }
    assert_eq!(
        (forever["owed"], forever["pending"], forever["looks"]),
        (1, 0, 0),
        "the promise stands and nothing asks"
    );
    assert!(!buggy.check_invariant("NoRetryPromisedPastTheBound", &forever));
}

/// The model's `UnreadLooks` bound.
fn unread_looks(model: &aterm_spec::derive::Model) -> i64 {
    model
        .consts
        .iter()
        .find(|(name, _)| *name == "UnreadLooks")
        .map(|(_, value)| *value)
        .expect("the model names its bound")
}
