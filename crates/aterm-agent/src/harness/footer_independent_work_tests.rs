// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::account_progress::{
    AccountProgress, ActiveSignIn, OwnClaimBlocker, OwnClaimFinding, ProgressPhase,
};

fn progress() -> AccountProgress {
    let mut value = AccountProgress::unknown(100);
    value.phase = ProgressPhase::Waiting;
    value.account = Some("primary-A".into());
    value.known = Some(3);
    value.eligible = Some(1);
    value.wall = Some("weekly".into());
    value.next_at = Some(500);
    value.own_claim = Some(OwnClaimFinding {
        blocker: OwnClaimBlocker::HeldElsewhere { until: 600 },
        pinned: true,
    });
    value
}

fn work() -> ActiveSignIn {
    ActiveSignIn {
        label: "pool-B".into(),
        route: Some("mailbox".into()),
        observed_at: 120,
    }
}

#[test]
fn independent_work_is_disclosed_without_changing_decision_or_forecast_authority() {
    let original = progress();
    let mut value = original.clone();
    value.independent_work.push(work());
    let text = account_progress_text(&value, false);
    assert!(text.contains("Waiting") && text.contains("pool-B") && text.contains("mailbox"));
    assert!(text.contains(&crate::harness::usage::rfc3339_utc(500)));
    assert_eq!(
        account_progress_phase(&value),
        account_progress_phase(&original)
    );
    let mut before = FooterFacts {
        account_progress: Some(original.clone()),
        ..Default::default()
    };
    before.glance = Some(crate::harness::outlook::Glance {
        title: Some("retained primary projection".into()),
        spoken: Some("The primary forecast is retained.".into()),
        ..Default::default()
    });
    let mut after = before.clone();
    after.account_progress = Some(value.clone());
    assert_eq!(
        after.account_forecast_refusal(),
        before.account_forecast_refusal()
    );
    assert_eq!(after.glance_view(), before.glance_view());
    value.independent_work.clear();
    assert_eq!(value, original);
}

#[test]
fn independent_work_survives_watching_and_narrow_composer_fitting() {
    for phase in [ProgressPhase::Waiting, ProgressPhase::Watching] {
        let mut value = progress();
        value.phase = phase;
        if value.phase == ProgressPhase::Watching {
            value.wall = None;
            value.known = None;
            value.eligible = None;
        }
        value.independent_work.push(work());
        let facts = FooterFacts {
            model: Some("a long model name that yields to recovery".into()),
            path: Some("/a/long/path/that/yields/to/recovery".into()),
            account_progress: Some(value.clone()),
            ..Default::default()
        };
        assert!(facts.account_progress_on_rule().is_some());
        assert!(account_progress_text(&value, true).contains("pool-B"));
        for room in [60, 120] {
            let fit = fit_rule(&facts, room, None);
            assert!(fit.width(None) <= room);
            assert!(
                fit.segments
                    .iter()
                    .any(|segment| segment.text.contains("pool-B")
                        && segment.text.contains("sign-in"))
            );
        }
    }
}

#[test]
fn a_busy_view_withdraws_current_pool_work_without_erasing_the_record() {
    let mut value = progress();
    value.independent_work.push(work());
    let facts = FooterFacts {
        account_progress: Some(value.clone()),
        account_progress_busy: true,
        ..Default::default()
    };
    let view = facts.account_progress_view().unwrap();
    assert!(view.independent_work.is_empty());
    assert_eq!(view.phase, ProgressPhase::Unknown);
    assert_eq!(view.next_at, None);
    assert_eq!(view.eligible, None);
    assert_eq!(
        facts.account_forecast_refusal(),
        Some(crate::harness::outlook::Refusal::ObserverBusy)
    );
    assert_eq!(facts.account_progress, Some(value));
}

#[test]
fn long_work_and_primary_targets_still_fit_the_narrow_recovery_row() {
    let mut value = progress();
    value.phase = ProgressPhase::Switching { to: "a".repeat(80) };
    let mut work = work();
    work.label = format!("pool-{}", "b".repeat(80));
    value.independent_work.push(work);
    let facts = FooterFacts {
        account_progress: Some(value),
        ..Default::default()
    };
    let fit = fit_rule(&facts, 60, None);
    assert!(fit.width(None) <= 60);
    assert!(
        fit.segments
            .iter()
            .any(|segment| segment.text.contains("Switching")
                && segment.text.contains("pool-")
                && segment.text.contains("sign-in"))
    );
    assert!(chips_room(&facts, 60) < 60);
}
