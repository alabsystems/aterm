// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: the real herald and App config edge cancel desktop-only work.

use super::{Escalation, EscalationKind, Herald, NOTIFY_BURST, NOTIFY_BURST_WINDOW};
use aterm_spec::derive::desktop_alert_debt_model;
use aterm_spec::interp::State;
use std::collections::BTreeSet;
use std::time::Instant;

struct Rig {
    app: crate::App,
    now: Instant,
    next_prime: u64,
    notice: bool,
}

fn escalation(session: u64, key: u64) -> Escalation {
    Escalation {
        session,
        kind: EscalationKind::Attention,
        key,
        label: key.to_string(),
        body: "attention".to_string(),
        shared: false,
    }
}

impl Rig {
    fn new() -> Self {
        Self {
            app: crate::App::headless_for_test(),
            now: Instant::now(),
            next_prime: 1,
            notice: false,
        }
    }

    fn reset(&mut self) {
        self.app.presence.herald = Herald::default();
        self.app.config.desktop_alerts = None;
        self.app.apply_desktop_alerts(false);
        self.now = Instant::now();
        self.next_prime = 1;
        self.notice = false;
    }

    fn project(&self) -> State {
        let slot = self.app.presence.herald.slots.get(&0);
        State::from([
            (
                "enabled",
                i64::from(self.app.config.desktop_alerts_or_default()),
            ),
            (
                "label",
                slot.and_then(|s| s.label.as_ref())
                    .map_or(0, |s| s.parse().unwrap()),
            ),
            (
                "seen",
                slot.and_then(|s| s.shown).map_or(0, |(_, key)| key as i64),
            ),
            ("pending", i64::from(slot.is_some_and(|s| s.owed > 0))),
            ("notice", i64::from(self.notice)),
        ])
    }

    fn step(&mut self, action: &str) {
        self.notice = false;
        match action {
            "Enable" | "Disable" => {
                let enabled = action == "Enable";
                self.app.config.desktop_alerts = Some(enabled);
                self.app.apply_desktop_alerts(enabled);
                assert_eq!(
                    self.app
                        .notify_own_alerts
                        .load(std::sync::atomic::Ordering::Acquire),
                    enabled,
                );
            }
            "ObserveNewLimited" | "ObserveNewFree" | "Reobserve" | "Tick" => {
                if action == "ObserveNewLimited" {
                    // Condition the unmodeled rate limiter through genuine
                    // notices for other sessions, as recent enabled output
                    // does. Never write its counters or timestamps directly.
                    for _ in 0..NOTIFY_BURST {
                        let session = self.next_prime;
                        self.next_prime += 1;
                        self.app.presence.herald.note(
                            session,
                            Some(&escalation(session, 1)),
                            false,
                            true,
                            self.now,
                        );
                    }
                } else {
                    self.now += NOTIFY_BURST_WINDOW;
                }
                let label = self.project()["label"] as u64;
                let key = if action.starts_with("ObserveNew") {
                    if label == 1 { 2 } else { 1 }
                } else {
                    label
                };
                let current = (key > 0).then(|| escalation(0, key));
                let out = self.app.presence.herald.note(
                    0,
                    current.as_ref(),
                    false,
                    self.app.config.desktop_alerts_or_default(),
                    self.now,
                );
                self.notice = out.notice.is_some();
            }
            _ => unreachable!("{action}"),
        }
    }
}

#[test]
fn desktop_alert_debt_conforms_to_real_herald_and_config_transitions() {
    let model = desktop_alert_debt_model();
    let actions = [
        "Enable",
        "Disable",
        "ObserveNewLimited",
        "ObserveNewFree",
        "Reobserve",
        "Tick",
    ];
    let mut visited = BTreeSet::new();
    // Reuse the App fixture, not its herald: every schedule starts with fresh
    // debt and keys without starting thousands of unrelated worker threads.
    let mut rig = Rig::new();
    for mut schedule in 0..actions.len().pow(5) {
        rig.reset();
        let mut state = model.init_state();
        assert_eq!(rig.project(), state);
        for _ in 0..5 {
            let action = actions[schedule % actions.len()];
            schedule /= actions.len();
            if !model.action_enabled(action, &state) {
                break;
            }
            rig.step(action);
            assert!(model.fire(action, &mut state));
            assert_eq!(rig.project(), state, "after {action}");
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &state),
                    "{}",
                    invariant.name
                );
            }
            visited.insert(action);
        }
    }
    assert_eq!(visited, BTreeSet::from(actions));

    // Historical policy order: the herald ran enabled, then its caller
    // suppressed the returned notice. The fourth session was rate-limited,
    // so no returned notice existed to suppress; a retry was still owed.
    let now = Instant::now();
    let mut old_order = Herald::default();
    let mut fixed = Herald::default();
    for session in 0..=NOTIFY_BURST as u64 {
        let esc = escalation(session, 1);
        let _discarded = old_order.note(session, Some(&esc), false, true, now);
        assert!(
            fixed
                .note(session, Some(&esc), false, false, now)
                .notice
                .is_none()
        );
    }
    let later = now + NOTIFY_BURST_WINDOW;
    assert_eq!(
        old_order.due(later).0.len(),
        1,
        "negative control reproduces debt"
    );
    assert!(
        old_order.due(now).1.is_some(),
        "negative control arms a wake"
    );
    assert_eq!(fixed.due(later), (vec![], None));
    assert_eq!(fixed.due(now), (vec![], None));
    eprintln!(
        "desktop-alert census: 4 disabled sessions; historical ordering owes 1 retry and arms a deadline; fixed ordering owes 0 and arms no deadline"
    );

    rig.reset();
    for action in ["Enable", "ObserveNewLimited"] {
        rig.step(action);
    }
    // An atomic-only config update was the other historical defect. Keep
    // the genuine owed notice and reproduce that old edge as a mutant.
    rig.app.config.desktop_alerts = Some(false);
    rig.app
        .notify_own_alerts
        .store(false, std::sync::atomic::Ordering::Release);
    assert!(!model.check_invariant("DisabledOwnsNoDebt", &rig.project()));
    rig.app.apply_desktop_alerts(false);
    assert!(model.check_invariant("DisabledOwnsNoDebt", &rig.project()));
}
