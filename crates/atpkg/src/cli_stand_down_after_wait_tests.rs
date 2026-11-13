// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{attempted_pass_index_build, pass_ended_since_queued, stood_down_after_wait};
use aterm_update_core::pkg_check::{PassOutcome, Stamps};

#[test]
fn a_waited_child_stands_down_behind_any_full_pass_that_ended_since_it_queued() {
    let now = 1_700_000_020;
    let ended = |outcome: Option<PassOutcome>, at: i64| Stamps {
        last_success: (outcome == Some(PassOutcome::Ok)).then_some(at),
        last_attempt: Some(at),
        last_outcome: outcome,
        ..Stamps::default()
    };
    for outcome in [PassOutcome::Ok, PassOutcome::Failed, PassOutcome::Offline] {
        assert_eq!(
            pass_ended_since_queued(true, Some(4), 5, &ended(Some(outcome), now - 8), now, None),
            Some((outcome, 8)),
            "{outcome:?}: the holder ended 8 s ago: stand down"
        );
    }
    assert_eq!(
        pass_ended_since_queued(
            true,
            Some(4),
            5,
            &ended(Some(PassOutcome::Ok), now),
            now,
            None
        ),
        Some((PassOutcome::Ok, 0)),
        "a same-second end is a moved count: stand down"
    );
    let failed = ended(Some(PassOutcome::Failed), now - 8);
    assert_eq!(
        pass_ended_since_queued(true, Some(5), 5, &failed, now, None),
        None,
        "no end since this child was queued (the record is the previous pass): run"
    );
    assert_eq!(
        pass_ended_since_queued(false, Some(4), 5, &failed, now, None),
        None,
        "a lock taken at the first try waited for nobody: run"
    );
    assert_eq!(
        pass_ended_since_queued(true, None, 5, &failed, now, None),
        None,
        "not a queued child (no count read at its start): run"
    );
    assert_eq!(
        pass_ended_since_queued(true, Some(0), 0, &failed, now, None),
        None,
        "a holder that counted nothing (an older atpkg): run"
    );
    assert_eq!(
        pass_ended_since_queued(
            true,
            Some(4),
            5,
            &ended(Some(PassOutcome::Ok), now + 30),
            now,
            None
        ),
        Some((PassOutcome::Ok, 0)),
        "a stamp ahead of the clock reads as just now"
    );
    assert!(stood_down_after_wait(PassOutcome::Ok, 8, "").contains("up to date"));
    assert!(stood_down_after_wait(PassOutcome::Offline, 8, "").contains("offline"));
    let line = stood_down_after_wait(PassOutcome::Failed, 8, "update failed: index unreachable");
    assert!(
        line.contains("(update failed: index unreachable)"),
        "{line}"
    );
    assert!(stood_down_after_wait(PassOutcome::Failed, 8, " ").contains("(it recorded no reason)"));
}

#[test]
fn queued_new_index_runs_after_older_pass_but_same_failed_target_stands_down() {
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};
    let model = aterm_models_atpkg::atpkg_published_spacing_model();
    let mut modeled = model.init_state();
    for action in ["EndOldOk", "Publish45", "DecideQueued"] {
        assert!(model.fire(action, &mut modeled), "{action}: {modeled:?}");
    }
    let first = 1_700_000_000;
    let ended = first + 12;
    let record = |outcome, target: Option<u64>| Stamps {
        last_success: (outcome == PassOutcome::Ok).then_some(ended),
        last_attempt: Some(ended),
        last_outcome: Some(outcome),
        last_pass_target: target.map(|build| (ended, build)),
        ..Stamps::default()
    };
    let actual = pass_ended_since_queued(
        true,
        Some(0),
        1,
        &record(PassOutcome::Ok, Some(44)),
        ended + 1,
        Some(45),
    );
    assert_eq!(actual.is_none(), modeled["queued"] == 1);
    assert_eq!(actual, None, "queued 45 runs after holder checked 44");
    for outcome in [PassOutcome::Ok, PassOutcome::Failed, PassOutcome::Offline] {
        assert_eq!(
            pass_ended_since_queued(
                true,
                Some(0),
                1,
                &record(outcome, Some(45)),
                ended + 1,
                Some(45)
            ),
            Some((outcome, 1)),
            "the holder already attempted 45: {outcome:?}"
        );
    }
    assert_eq!(
        pass_ended_since_queued(
            true,
            Some(0),
            1,
            &record(PassOutcome::Ok, None),
            ended + 1,
            Some(45)
        ),
        Some((PassOutcome::Ok, 1)),
        "an old record without a target fails closed"
    );
    assert_eq!(
        pass_ended_since_queued(
            true,
            Some(0),
            1,
            &record(PassOutcome::Failed, Some(44)),
            ended + 1,
            Some(45)
        ),
        None,
        "a newer 45 can run after an older 44 failed"
    );
}

#[test]
fn pass_target_needs_a_real_index_attempt_and_never_downgrades_the_requested_build() {
    assert_eq!(attempted_pass_index_build(45, 0, false), 0);
    assert_eq!(attempted_pass_index_build(45, 44, true), 45);
    assert_eq!(attempted_pass_index_build(44, 45, true), 45);
    assert_eq!(attempted_pass_index_build(0, 45, true), 45);
    assert_eq!(attempted_pass_index_build(45, 0, true), 45);
}
