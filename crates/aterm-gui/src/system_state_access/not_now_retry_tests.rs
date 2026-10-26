// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A panel's answer remains effective while its history write is unavailable.

use super::*;
use std::cell::Cell;

use crate::WindowId;
use crate::app_control::ActRequest;
use crate::devtools_helper::{Page, PanelEvent, RECHECK_EVERY, test_arms};
use crate::native_app::{ActionInvocation, AppEvent};
use crate::native_ui::ActionId;
use crate::tab_model::ViewId;

type Record = (u64, Access, AccessOutcome);

thread_local! {
    static WRITABLE: Cell<bool> = const { Cell::new(false) };
    static ATTEMPTS: RefCell<Vec<Record>> = const { RefCell::new(Vec::new()) };
    static SAVED: RefCell<Vec<Record>> = const { RefCell::new(Vec::new()) };
}

fn key(access: Access) -> &'static str {
    match access {
        Access::FullDiskAccess => "system-state/row/full-disk-access/open",
        Access::DeveloperTools => "system-state/row/developer-tools/open",
        _ => unreachable!(),
    }
}

fn page(access: Access) -> Page {
    match access {
        Access::FullDiskAccess => Page::FullDiskAccess,
        Access::DeveloperTools => Page::DeveloperTools,
        _ => unreachable!(),
    }
}

fn opens(access: Access) -> u32 {
    match access {
        Access::FullDiskAccess => test_arms::fda_counts().0,
        Access::DeveloperTools => test_arms::opens(),
        _ => unreachable!(),
    }
}

fn off(app: &mut App) {
    use aterm_containment::DevToolGrant;
    use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel, ProbeGate};
    fn no_probe(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
    }
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
        aterm_containment::consent::FdaState::Denied,
    ));
    app.consent
        .set_devtools_arms_for_test(no_probe, || Some(test_arms::SUBJECT));
    app.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
        Instant::now(),
    );
    let _ = app.devtools_watch.observe(DevToolGrant::NotGranted, true);
    app.publish_system_state(status(
        Selection::all(),
        [Access::FullDiskAccess, Access::DeveloperTools]
            .into_iter()
            .map(|access| observed(Subject::Access(access), Standing::NeedsYou, None))
            .collect(),
    ));
}

fn person(app: &mut App, wid: WindowId, access: Access) {
    off(app);
    let _ = app.take_system_state_press();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key(access)),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(app.devtools_helper.panel().unwrap().page(), page(access));
}

fn decline(app: &mut App, now: Instant) {
    app.devtools_helper.press_for_test(PanelEvent::NotNow);
    app.tick_devtools_hand_over(now);
    assert!(app.devtools_helper.panel().is_none());
    app.settle_access_steps(now);
}

fn client(app: &mut App, view: ViewId, access: Access, now: Instant) {
    off(app);
    app.act_app(ActRequest {
        view,
        ui_key: key(access).into(),
        action: key(access).into(),
        value: None,
    })
    .expect("the current row accepts the control action");
    app.tick_local_network_step(now);
    app.tick_devtools_hand_over(now);
}

fn declined(access: Access) -> (App, WindowId, ViewId, Instant) {
    WRITABLE.with(|writable| writable.set(false));
    ATTEMPTS.with(|records| records.borrow_mut().clear());
    SAVED.with(|records| records.borrow_mut().clear());
    set_front(true);
    let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
    app.access_steps.set_reach(StepsReach {
        record: |revision, access, outcome| {
            let record = (revision, access, outcome);
            ATTEMPTS.with(|records| records.borrow_mut().push(record));
            if !WRITABLE.with(Cell::get) {
                return Err(std::io::Error::other(
                    "injected access-history write failure",
                ));
            }
            SAVED.with(|records| records.borrow_mut().push(record));
            Ok(())
        },
        recorded: |revision, access| {
            if access == Access::LocalNetwork {
                return Some(AccessOutcome::Triggered);
            }
            SAVED.with(|records| {
                records
                    .borrow()
                    .iter()
                    .rev()
                    .find_map(|(at, row, outcome)| {
                        (*at == revision && *row == access).then_some(*outcome)
                    })
            })
        },
        frontmost: front,
        ..stepping()
    });
    assert!(app.open_system_state_tab());
    let wid = app.frontmost_window.unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();
    focus(&mut app, true);
    let now = Instant::now();
    app.tick_local_network_step(now);
    person(&mut app, wid, access);
    decline(&mut app, now);
    assert!(ATTEMPTS.with(|records| records.borrow().contains(&(
        1,
        access,
        AccessOutcome::Declined
    ))));
    assert!(SAVED.with(|records| records.borrow().is_empty()));
    assert!((app.devtools_helper.reach().last_declined)().is_none());
    assert!((app.devtools_helper.fda_reach().last_declined)().is_none());
    (app, wid, view, now)
}

#[test]
fn not_now_record_failure_holds_the_fda_client_retry() {
    let access = Access::FullDiskAccess;
    let (mut app, _, view, now) = declined(access);
    let before = opens(access);
    client(&mut app, view, access, now + RECHECK_EVERY);
    assert_eq!(
        opens(access),
        before,
        "the same approval already answered Not now"
    );
    assert!(app.devtools_helper.panel().is_none());
}

#[test]
fn not_now_record_failure_holds_the_developer_tools_client_retry() {
    let access = Access::DeveloperTools;
    let (mut app, _, view, now) = declined(access);
    let before = opens(access);
    client(&mut app, view, access, now + RECHECK_EVERY);
    assert_eq!(
        opens(access),
        before,
        "the same approval already answered Not now"
    );
    assert!(app.devtools_helper.panel().is_none());
}

#[test]
fn not_now_record_failure_a_later_grant_and_new_decline_replace_the_local_answer() {
    for access in [Access::FullDiskAccess, Access::DeveloperTools] {
        let (mut app, _, view, now) = declined(access);
        if access == Access::FullDiskAccess {
            assert!(app.fda_declined_in_revision());
            app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(probe(
                aterm_containment::consent::FdaState::Granted,
            ));
            app.note_macos_access_granted(now);
            assert!(
                !app.fda_declined_in_revision(),
                "the local answer is not a cached decline"
            );
        } else {
            let step = app
                .devtools_watch
                .observe(aterm_containment::DevToolGrant::Granted, true);
            assert!(!app.apply_devtools_step(step));
        }
        assert!(ATTEMPTS.with(|records| records.borrow().contains(&(
            1,
            access,
            AccessOutcome::Granted
        ))));
        assert!(SAVED.with(|records| records.borrow().is_empty()));
        let before = opens(access);
        client(&mut app, view, access, now + RECHECK_EVERY);
        assert_eq!(
            opens(access),
            before + 1,
            "a later grant retired the local decline"
        );
        decline(&mut app, now + RECHECK_EVERY);
        let before = opens(access);
        client(&mut app, view, access, now + RECHECK_EVERY * 2);
        assert_eq!(opens(access), before, "the new Not now is effective again");
        assert!(app.devtools_helper.panel().is_none());
    }
}

#[test]
fn not_now_record_failure_keeps_owner_new_approval_and_other_rows_independent() {
    for access in [Access::FullDiskAccess, Access::DeveloperTools] {
        let (mut app, wid, view, now) = declined(access);
        let due = app
            .next_access_step_deadline(now)
            .expect("the failed answer needs saving");
        WRITABLE.with(|writable| writable.set(true));
        let before = opens(access);
        app.settle_access_steps(due);
        assert!(SAVED.with(|records| records.borrow().contains(&(
            1,
            access,
            AccessOutcome::Declined
        ))));
        let saved = SAVED.with(|records| records.borrow().clone());
        app.settle_access_steps(due + super::super::ACCESS_RECORD_RETRY);
        assert_eq!(SAVED.with(|records| records.borrow().clone()), saved);
        assert_eq!(opens(access), before, "saving the answer opens no pane");

        person(&mut app, wid, access);
        assert_eq!(opens(access), before + 1, "a person's press opens again");
        decline(&mut app, due);
        let other = if access == Access::FullDiskAccess {
            Access::DeveloperTools
        } else {
            Access::FullDiskAccess
        };
        let before_other = opens(other);
        client(&mut app, view, other, due + RECHECK_EVERY);
        assert_eq!(opens(other), before_other + 1, "another row is independent");
        decline(&mut app, due + RECHECK_EVERY);

        approve(&mut app, 2);
        let before = opens(access);
        client(&mut app, view, access, due + RECHECK_EVERY * 2);
        assert_eq!(
            opens(access),
            before + 1,
            "a new approval permits its own page"
        );
        assert_eq!(app.devtools_helper.panel().unwrap().page(), page(access));
    }
}
