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
    let text = account_progress_text(&value);
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
    before.quota = Some(crate::harness::quota_view::QuotaView {
        title: "retained primary projection".into(),
        spoken: "The primary forecast is retained.".into(),
        ..Default::default()
    });
    let mut after = before.clone();
    after.account_progress = Some(value.clone());
    assert_eq!(
        after.account_forecast_refusal(),
        before.account_forecast_refusal()
    );
    assert_eq!(after.quota_view(), before.quota_view());
    value.independent_work.clear();
    assert_eq!(value, original);
}

/// Independent pool work is said where accounts are (the card's words and the quota row's
/// title's rotation clause, §5.1), watching or waiting, and never on the rule.
#[test]
fn independent_work_is_said_where_accounts_are_and_not_on_the_rule() {
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
            model: Some("a long model name".into()),
            path: Some("/a/long/path".into()),
            account_progress: Some(value.clone()),
            ..Default::default()
        };
        assert!(account_progress_text(&value).contains("pool-B"));
        let rotation = rotation_words(&value, |at| at.to_string());
        assert!(
            rotation.contains("local pool sign-in: pool-B via mailbox"),
            "{rotation}"
        );
        for room in [60, 120] {
            let fit = fit_rule(&facts, room, None);
            assert!(fit.width(None) <= room);
            assert!(
                !fit.segments
                    .iter()
                    .any(|segment| segment.text.contains("pool-B")),
                "{room}: {fit:?}"
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
