// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Consent diagnostics remain available without becoming desktop alerts.

use std::collections::HashSet;
use std::time::Instant;

use super::{NotifyMsg, delivers};
use crate::consent_observer::AttentionEvent;

#[test]
fn consent_failure_and_recovery_never_require_a_desktop_notice() {
    use crate::App;
    use crate::consent_state::ConsentState;
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};

    let mut app = App::headless_for_test();
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        notice: Some(false),
        ..Default::default()
    });
    app.config.desktop_alerts = Some(true);
    let (notices, posted) = crate::notify::Notifier::channel();
    app.session_factory.notify_tx = notices;
    app.note_consent_attention(AttentionEvent::ProtectedEperm { session: None });
    let queued = posted
        .try_recv()
        .expect("diagnostic reaches the delivery policy");
    assert!(app.consent_attention.has_denials());
    assert!(!delivers(&queued, &HashSet::new(), true));
    app.consent = ConsentState::with_cached_probe_for_test(FdaProbe {
        state: FdaState::Granted,
        label: ProbeLabel::OpenOk,
    });
    app.on_consent_probe_ready(Instant::now());
    assert!(!app.consent_attention.has_denials());
    assert!(!delivers(&queued, &HashSet::new(), true));
}

/// A tab's notification flood leaves room for the consent notice in the one process-wide queue.
#[test]
fn a_program_flood_does_not_crowd_out_the_consent_notice() {
    use crate::App;

    let mut app = App::headless_for_test();
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        notice: Some(false),
        ..Default::default()
    });
    app.config.desktop_alerts = Some(true);
    let (notices, posted) = super::Notifier::channel();
    app.session_factory.notify_tx = notices.clone();
    let flooded = (0..1_000)
        .filter(|_| {
            notices
                .try_send(NotifyMsg::program(7, None, "flood".into()))
                .is_ok()
        })
        .count();
    assert!(flooded > 0, "the flood fills what it may");
    app.note_consent_attention(AttentionEvent::ProtectedEperm { session: None });
    let own: Vec<NotifyMsg> = posted.try_iter().filter(|notice| notice.own).collect();
    assert_eq!(own.len(), 1, "the consent notice was queued");
    assert_eq!(own[0].session, u64::MAX);
}
