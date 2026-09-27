// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::native_update_structural_latch_model, interp, verify};

/// A STRUCTURAL convergence no longer strands every later release (gap 14,
/// 2026-09-26). The latch on build N gets ONE re-sample after a day and one
/// attempt per newer verified release — never the launch its boot trial
/// reverts on, which is held and said instead, and never PROMISED past the
/// first count that rules it out (gap 14 review) — and build N stays covered
/// whatever the digest. Tier-0 here; the Tier-1 bind to the real
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

    // The trial has room: a newer release releases the latch once, and the
    // attempt failing re-latches without a second one for the same release.
    let mut room = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut room));
    assert!(model.fire("NewerArrives", &mut room));
    assert!(model.fire("Decide", &mut room));
    assert_eq!((room["latched"], room["newer_spent"]), (0, 1));
    assert!(model.fire("AttemptFails", &mut room));
    assert!(
        !model.action_enabled("Decide", &room),
        "nothing new to decide"
    );
    // The day passes: the one re-sample, then never again.
    assert!(model.fire("DayPasses", &mut room));
    assert!(model.fire("Decide", &mut room));
    assert_eq!((room["latched"], room["owed"], room["spent"]), (0, 0, 1));
    assert!(model.fire("AttemptFails", &mut room));
    assert_eq!(room["owed"], 0, "a failed re-sample owes no second one");
    assert!(!model.action_enabled("DayPasses", &room));

    // Both at once: ONE attempt answers both.
    let mut both = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut both));
    assert!(model.fire("DayPasses", &mut both));
    assert!(model.fire("NewerArrives", &mut both));
    assert!(model.fire("Decide", &mut both));
    assert_eq!(
        (both["latched"], both["owed"], both["newer_spent"]),
        (0, 0, 1)
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

    // No room: the earned attempt is held and said, the latch stays, and the
    // hold spends the re-sample too — every launch left is the reverting one.
    let mut spent = model.init_state();
    assert!(model.fire("TrialIsSpent", &mut spent));
    assert!(model.fire("NewerArrives", &mut spent));
    assert!(model.fire("Decide", &mut spent));
    assert_eq!((spent["latched"], spent["said"], spent["owed"]), (1, 1, 0));
    assert!(model.fire("DayPasses", &mut spent));
    assert!(model.fire("Decide", &mut spent));
    assert_eq!(spent["latched"], 1, "the day finds nothing left to spend");

    // Build N stays latched whatever the digest.
    let mut same = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut same));
    assert!(model.fire("ArmSameBuild", &mut same));
    assert_eq!((same["latched"], same["escaped"]), (1, 0));

    // NEGATIVE CONTROL, one arc per mutant.
    let buggy = interp::with_buggy(&model, 1);
    // Today's strand: the newer release is blocked forever and the day
    // changes nothing.
    for (event, invariant) in [
        ("NewerArrives", "NewerReleaseIsTried"),
        ("DayPasses", "ResampleAfterADay"),
    ] {
        let mut strand = buggy.init_state();
        assert!(buggy.fire("TrialHasRoom", &mut strand));
        assert!(buggy.fire(event, &mut strand));
        assert!(buggy.fire("Decide", &mut strand));
        assert_eq!(strand["latched"], 1, "{event}: the strand holds");
        assert!(!buggy.check_invariant(invariant, &strand), "{invariant}");
    }
    // The trial-blind release: it spends the reverting launch, silently.
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
}
