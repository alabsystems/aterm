// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn authentication_and_context_failures_do_not_become_quota_exhaustion() {
    assert!(
        wall_name(AccountPoint::Wall {
            kind: aterm_phase::WallKind::UsageWeekly
        })
        .is_some()
    );
    for kind in [
        aterm_phase::WallKind::Auth,
        aterm_phase::WallKind::Context,
        aterm_phase::WallKind::Memory,
        aterm_phase::WallKind::Overloaded,
    ] {
        let point = AccountPoint::Wall { kind };
        assert_eq!(wall_name(point), None);
        assert_eq!(progress_phase(None, point), ProgressPhase::Waiting);
    }
}

#[test]
fn a_plan_to_move_is_not_a_report_that_a_move_started() {
    assert_eq!(
        progress_phase(
            Some(&AccountAdvice::Move {
                to: "other".into(),
                yields: None
            }),
            AccountPoint::TurnEnd
        ),
        ProgressPhase::Ready
    );
    assert_eq!(
        progress_phase(
            Some(&AccountAdvice::Probe { otherwise: None }),
            AccountPoint::TurnEnd
        ),
        ProgressPhase::Watching
    );
    assert_eq!(
        progress_phase(
            Some(&AccountAdvice::Again {
                until: Instant::now(),
                why: AskAgain::Unwritten,
            }),
            AccountPoint::TurnEnd
        ),
        ProgressPhase::Waiting,
        "the host's own ask again is a wait"
    );
    assert_eq!(
        progress_phase(None, AccountPoint::LoginEnded),
        ProgressPhase::Waiting
    );
    assert_eq!(
        progress_phase(None, AccountPoint::TurnEnd),
        ProgressPhase::Watching
    );
}

#[test]
fn a_progress_observation_cannot_cross_a_process_or_conversation() {
    assert!(same_progress_process(
        42,
        Some(100),
        Some("session"),
        42,
        Some(100),
        Some("session")
    ));
    for (pid, born, session) in [
        (43, Some(100), Some("session")),
        (42, Some(101), Some("session")),
        (42, None, Some("session")),
        (42, Some(100), Some("other")),
        (42, Some(100), None),
    ] {
        assert!(!same_progress_process(
            42,
            Some(100),
            Some("session"),
            pid,
            born,
            session
        ));
    }
    assert!(!same_progress_process(
        42,
        None,
        Some("session"),
        42,
        None,
        Some("session")
    ));
}
