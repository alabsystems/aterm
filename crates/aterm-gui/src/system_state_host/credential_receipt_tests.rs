// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::app_control::ActRequest;

const JOIN_SOURCE: &str = "http://joined.invalid/v1/team";
const OTHER_SOURCE: &str = "http://other.invalid/v1/team";
const PASSWORD: &str = "inert-join-password";
const REPLACEMENT: &str = "inert-new-password";

thread_local! {
    static SUBMISSIONS: RefCell<Vec<(Plan, bool, Option<SecretBytes>)>> = const { RefCell::new(Vec::new()) };
}

fn capture_submissions(app: &mut App) {
    SUBMISSIONS.with(|submissions| submissions.borrow_mut().clear());
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        apply: |plan, typed, password, receipt| {
            SUBMISSIONS.with(|submissions| {
                submissions.borrow_mut().push((plan, typed, password));
            });
            APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().push(receipt));
            true
        },
        ..handlers()
    });
}

fn password_projection(app: &mut App) -> String {
    app.native_runtime
        .system_state_mut()
        .unwrap()
        .field_projection("system-state/password")
        .unwrap()
        .text
}

fn assert_consumed_projection(app: &mut App) {
    let projection = app
        .native_runtime
        .system_state_mut()
        .unwrap()
        .field_projection("system-state/password")
        .unwrap();
    assert!(
        projection.text.is_empty(),
        "acceptance consumes the value before a receipt wake"
    );
    for at in [projection.selection.anchor, projection.selection.head] {
        assert!(
            projection.text.is_char_boundary(at),
            "the immediate field projection has a valid selection"
        );
    }
}

fn start_join(app: &mut App, wid: WindowId, view: ViewId) -> InitialSetupApplyReceipt {
    type_into(app, wid, view, "system-state/url", JOIN_SOURCE);
    type_into(app, wid, view, "system-state/password", PASSWORD);
    press(app, wid, view, "system-state/vault/join");
    assert!(SUBMISSIONS.with(|submissions| {
        let submissions = submissions.borrow();
        submissions.len() == 1
            && submissions[0].0.vault_url == JOIN_SOURCE
            && submissions[0].1
            && submissions[0].2.as_ref().and_then(SecretBytes::as_str) == Some(PASSWORD)
    }));
    receipt()
}

fn preempt_join(app: &mut App, view: ViewId) -> InitialSetupApplyReceipt {
    // This is a live enabled control while Join waits for its receipt; group
    // preemption must remain available during an admission or running pass.
    let key = "system-state/group/2/leave";
    app.act_app(ActRequest {
        view,
        ui_key: key.into(),
        action: key.into(),
        value: None,
    })
    .unwrap();
    assert!(SUBMISSIONS.with(|submissions| {
        let submissions = submissions.borrow();
        submissions.len() == 2 && !submissions[1].1 && submissions[1].2.is_none()
    }));
    receipt()
}

fn finish_latest(app: &mut App, receipt: InitialSetupApplyReceipt, source: &str) {
    let mut plan = app
        .system_state_host
        .pending_apply
        .as_ref()
        .unwrap()
        .plan
        .clone();
    receipt.accept();
    app.resolve_system_state_apply();
    // A group choice retains the source that its preceding Join admitted.
    plan.vault_url = source.into();
    fresh(
        app,
        SetupStatus {
            plan: Some(plan),
            ..SetupStatus::default()
        },
    );
}

fn accepted_password_survives_preemption(accept_before: bool) {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    capture_submissions(&mut app);
    let mut joined = Some(start_join(&mut app, wid, view));
    if accept_before {
        joined.take().unwrap().accept();
        assert_consumed_projection(&mut app);
    }
    // No receipt wake or periodic tick has run before this next queued input.
    let group = preempt_join(&mut app, view);
    if let Some(joined) = joined {
        joined.accept();
        assert_consumed_projection(&mut app);
    }
    finish_latest(&mut app, group, JOIN_SOURCE);
    assert!(
        password_projection(&mut app).is_empty(),
        "an accepted Join consumes its password even when a later group owns the plan receipt"
    );

    type_into(&mut app, wid, view, "system-state/url", OTHER_SOURCE);
    press(&mut app, wid, view, "system-state/vault/join");
    assert!(
        SUBMISSIONS.with(|submissions| {
            let submissions = submissions.borrow();
            let (plan, typed, password) = submissions.last().unwrap();
            plan.vault_url == OTHER_SOURCE && *typed && password.is_none()
        }),
        "a later source never receives the earlier Join's password"
    );
    drop(receipt());
    app.resolve_system_state_apply();
    assert!(app.structural_invariants_ok());
}

#[test]
fn credential_receipt_accepted_before_preemption_consumes_the_password() {
    accepted_password_survives_preemption(true);
}

#[test]
fn credential_receipt_accepted_after_preemption_consumes_the_password() {
    accepted_password_survives_preemption(false);
}

#[test]
fn credential_receipt_refused_or_overtaken_without_acceptance_keeps_the_draft() {
    for preempt in [false, true] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        capture_submissions(&mut app);
        let joined = start_join(&mut app, wid, view);
        if preempt {
            let group = preempt_join(&mut app, view);
            drop(joined);
            finish_latest(&mut app, group, &Plan::default().vault_url);
        } else {
            drop(joined);
            app.resolve_system_state_apply();
            fresh(
                &mut app,
                SetupStatus {
                    plan: Some(Plan::default()),
                    ..SetupStatus::default()
                },
            );
        }
        assert_eq!(password_projection(&mut app), "•".repeat(PASSWORD.len()));
        press(&mut app, wid, view, "system-state/vault/join");
        assert!(SUBMISSIONS.with(|submissions| {
            submissions
                .borrow()
                .last()
                .unwrap()
                .2
                .as_ref()
                .and_then(SecretBytes::as_str)
                == Some(PASSWORD)
        }));
        drop(receipt());
        app.resolve_system_state_apply();
        assert!(app.structural_invariants_ok());
    }
}

fn late_acceptance_after_refused_preemption(replace_password: bool) {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    capture_submissions(&mut app);
    let joined = start_join(&mut app, wid, view);
    let group = preempt_join(&mut app, view);
    // The later worker did not start. The earlier admission can still return
    // its receipt; it may be durably admitted but stopped before its first step.
    drop(group);
    app.resolve_system_state_apply();
    fresh(
        &mut app,
        SetupStatus {
            plan: Some(Plan::default()),
            ..SetupStatus::default()
        },
    );
    if replace_password {
        type_into(&mut app, wid, view, "system-state/password", REPLACEMENT);
    }
    let current = app.system_state_host.latest_status.plan.clone();
    joined.accept();
    // No host tick or receipt resolver runs between the late acceptance and
    // this new Join. Cleanup must belong to the exact password it submitted,
    // independently of which plan now owns the ordinary admission receipt.
    type_into(&mut app, wid, view, "system-state/url", OTHER_SOURCE);
    press(&mut app, wid, view, "system-state/vault/join");
    SUBMISSIONS.with(|submissions| {
        let submissions = submissions.borrow();
        let (plan, typed, password) = submissions.last().unwrap();
        assert_eq!(plan.vault_url, OTHER_SOURCE);
        assert!(*typed);
        assert_eq!(
            password.as_ref().and_then(SecretBytes::as_str),
            replace_password.then_some(REPLACEMENT)
        );
    });
    assert_eq!(
        app.system_state_host.latest_status.plan, current,
        "a superseded receipt must not adopt its old plan"
    );
    drop(receipt());
    app.resolve_system_state_apply();
    assert!(app.structural_invariants_ok());
}

#[test]
fn credential_receipt_late_acceptance_cannot_resend_the_old_password_before_a_tick() {
    late_acceptance_after_refused_preemption(false);
}

#[test]
fn credential_receipt_late_acceptance_preserves_a_new_password_draft() {
    late_acceptance_after_refused_preemption(true);
}
