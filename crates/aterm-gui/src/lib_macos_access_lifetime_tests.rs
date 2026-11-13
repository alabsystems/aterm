// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::consent_card::{CardPhase, Marker, Verdict, WATCH_FOR};
use super::consent_state::ConsentState;
use super::macos_access_refresh_tests::{access_row, grant_recorded, windowed};
use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
use std::time::Instant;

#[test]
fn pending_access_cannot_extend_any_waiting_phase() {
    let now = Instant::now();
    for phase in ["due", "watching"] {
        let mut app = windowed(FdaProbe::pending());
        assert!(app.consent_card.begin_deciding(now));
        assert!(app.consent_card.on_decided(&Verdict::Offer, now));
        if phase == "watching" {
            app.consent_card.on_raised(now);
        }
        app.tick_macos_access_card(now + WATCH_FOR);
        assert_eq!(app.consent_card.phase(), CardPhase::Settled, "{phase}");
        assert!(
            !access_row(&app) && !grant_recorded(&app),
            "an expired phase cannot offer or confirm"
        );
        assert_eq!(
            app.next_macos_access_card_deadline(now + WATCH_FOR),
            None,
            "an expired phase owns no timer"
        );
    }
}

#[test]
fn timed_out_initial_probe_drops_its_deferred_marker_before_retry() {
    let now = Instant::now();
    let mut app = windowed(FdaProbe::pending());
    assert!(app.consent_card.begin_deciding(now));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(Some(
        Marker::Opened,
    )));
    assert_eq!(
        app.consent_card_host.pending,
        Some(crate::consent_card::PendingDecision::launch(Some(
            Marker::Opened
        )))
    );
    let deadline = app.consent_card.lifecycle_deadline().unwrap();
    assert!(app.next_macos_access_card_deadline(now).unwrap() <= deadline);
    app.tick_macos_access_card(deadline);
    assert_eq!(
        app.consent_card.phase(),
        CardPhase::Settled,
        "the retry starts on this wake and settles because this fixture has no worker proxy"
    );
    assert_eq!(app.consent_card_host.pending, None);
    assert!(!access_row(&app) && !grant_recorded(&app));
    app.consent = ConsentState::with_cached_probe_for_test(FdaProbe {
        state: FdaState::Granted,
        label: ProbeLabel::OpenOk,
    });
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(Some(
        Marker::Opened,
    )));
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert_eq!(app.consent_card_host.pending, None);
    assert!(!grant_recorded(&app));
}
