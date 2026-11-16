// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::title_authority::{Fault, with_fault};

fn split_fixture() -> (App, tab_model::ViewId, u64, Instant, u64) {
    let mut app = App::headless_for_test();
    app.split_active_stub_tab(WindowId(0));
    let tab = app.windows[&WindowId(0)].tab_set.active().unwrap();
    let hidden = tab
        .root
        .leaves()
        .into_iter()
        .find(|view| *view != tab.focus)
        .unwrap();
    let sid = app
        .view_store
        .get(hidden)
        .copied()
        .and_then(tab_model::View::terminal_session)
        .unwrap();
    app.pool
        .get(sid)
        .unwrap()
        .term
        .lock()
        .unwrap()
        .process(b"\x1b]2;joined immediately\x07");
    let now = Instant::now();
    for _ in 0..4 {
        app.observe_title_drift(sid, now);
    }
    let epoch = app
        .pool
        .get(sid)
        .unwrap()
        .term
        .lock()
        .unwrap()
        .title_epoch();
    assert!(
        !app.windows[&WindowId(0)]
            .tab_title_epochs
            .contains_key(&sid)
    );
    assert!(
        app.title_drift.completed.contains_key(&sid),
        "negative membership can complete"
    );
    (app, hidden, sid, now, epoch)
}

fn published_join(app: &mut App, sid: u64, epoch: u64, last_flush: Instant) {
    let wid = WindowId(0);
    let titles = app.tab_titles(wid);
    assert!(titles.iter().any(|title| title == "joined immediately"));
    assert_eq!(
        app.windows[&wid].tab_title_epochs.get(&sid),
        Some(&epoch),
        "publication must deliver a same-epoch join without waiting for another output or debounce"
    );
    assert_eq!(
        app.title_drift.last_flush[&sid], last_flush,
        "topology cannot reset ordinary pacing"
    );
}

#[test]
fn title_authority_same_epoch_focus_is_delivered_before_headless_publication() {
    let run = |fault: bool| {
        let (mut app, hidden, sid, now, epoch) = split_fixture();
        let last = app.title_drift.last_flush[&sid];
        assert!(app.title_drift.due_at(sid).unwrap() > now);
        let mut change = || {
            assert!(
                app.windows
                    .get_mut(&WindowId(0))
                    .unwrap()
                    .tab_set
                    .active_mut()
                    .unwrap()
                    .set_focus(hidden)
            );
            published_join(&mut app, sid, epoch, last);
        };
        if fault {
            tab_model::with_title_membership_fault(change);
        } else {
            change();
        }
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}

#[test]
fn title_authority_publication_boundary_detects_missing_reconciliation() {
    let (mut app, hidden, sid, _, epoch) = split_fixture();
    let last = app.title_drift.last_flush[&sid];
    app.windows
        .get_mut(&WindowId(0))
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(hidden);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_fault(Fault::Reconcile, || {
                published_join(&mut app, sid, epoch, last)
            });
        }))
        .is_err()
    );
}

#[test]
fn title_authority_same_ids_replaced_tabset_has_fresh_owner_and_revokes_old_one() {
    let run = |fault: bool| {
        let (mut app, hidden, sid, _, epoch) = split_fixture();
        let last = app.title_drift.last_flush[&sid];
        // A clone intentionally preserves every logical ID while obtaining a new owner lease.
        let mut replacement = app.windows[&WindowId(0)].tab_set.clone();
        replacement.active_mut().unwrap().set_focus(hidden);
        let change = || {
            let mut retired = {
                let mut ws = app.windows.get_mut(&WindowId(0)).unwrap();
                std::mem::replace(&mut ws.tab_set, replacement)
            };
            published_join(&mut app, sid, epoch, last);
            let revision = app.windows.title_authority().revision();
            let _ = retired.active_mut();
            assert!(
                revision.is_current(app.windows.title_authority()),
                "detached old root cannot dirty its former owner"
            );
        };
        if fault {
            with_fault(Fault::Root, change);
        } else {
            change();
        }
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}

#[test]
fn title_authority_view_store_replacement_with_same_view_ids_is_a_publication_boundary() {
    let run = |fault: bool| {
        let (mut app, hidden, sid, _, epoch) = split_fixture();
        let last = app.title_drift.last_flush[&sid];
        let focused = app.windows[&WindowId(0)].tab_set.active().unwrap().focus;
        let mut current: Vec<_> = app.view_store.iter().collect();
        current.sort_by_key(|(id, _)| *id);
        let mut store = tab_model::ViewStore::default();
        let other = app
            .view_store
            .get(focused)
            .copied()
            .and_then(tab_model::View::terminal_session)
            .unwrap();
        for (id, _) in current {
            let inserted = store
                .insert_terminal(if id == focused { sid } else { other })
                .unwrap();
            assert_eq!(
                inserted, id,
                "same numeric identities in an independent store"
            );
        }
        assert_ne!(focused, hidden);
        let change = || {
            app.view_store = store;
            published_join(&mut app, sid, epoch, last);
        };
        if fault {
            with_fault(Fault::View, change);
        } else {
            change();
        }
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}

fn delivered_fixture() -> (App, Instant, u64) {
    let mut app = App::headless_for_test();
    app.pool
        .get(0)
        .unwrap()
        .term
        .lock()
        .unwrap()
        .process(b"\x1b]2;delivered\x07");
    let now = Instant::now();
    for _ in 0..4 {
        app.observe_title_drift(0, now);
    }
    let epoch = app.title_drift.seen[&0];
    assert!(app.title_drift.completed.contains_key(&0));
    (app, now, epoch)
}

#[test]
fn title_authority_independent_delivery_removal_and_root_replacement_are_owed() {
    let run = |fault: Option<Fault>, replace: bool| {
        let (mut app, _, epoch) = delivered_fixture();
        let last = app.title_drift.last_flush[&0];
        let mut change = || {
            {
                let mut ws = app.windows.get_mut(&WindowId(0)).unwrap();
                if replace {
                    ws.tab_title_epochs = crate::title_authority::TitleEpochs::default();
                } else {
                    ws.tab_title_epochs.remove(&0);
                }
            }
            let _ = app
                .inspect_app(crate::app_control::InspectRequest::Tabs)
                .unwrap();
            assert_eq!(
                app.windows[&WindowId(0)].tab_title_epochs.get(&0),
                Some(&epoch)
            );
            assert_eq!(app.title_drift.last_flush[&0], last);
        };
        if let Some(fault) = fault {
            with_fault(fault, change);
        } else {
            change();
        }
    };
    run(None, false);
    run(None, true);
    for (fault, replace) in [(Fault::Delivery, false), (Fault::Root, true)] {
        assert!(std::panic::catch_unwind(|| run(Some(fault), replace)).is_err());
    }
}

#[test]
fn title_authority_pending_work_excludes_a_completed_fast_path() {
    let run = |fault: bool| {
        let (mut app, now, _) = delivered_fixture();
        app.title_drift.pending.insert(0);
        let _ = crate::work_counts::take_title_refresh();
        let mut observe = || {
            app.observe_title_drift(0, now);
            assert!(
                crate::work_counts::take_title_refresh().window_gates > 0,
                "an owed observation cannot use a completion certificate"
            );
            assert!(app.title_drift.pending.contains(&0));
        };
        if fault {
            with_fault(Fault::Pending, observe);
        } else {
            observe();
        }
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}

#[test]
fn title_authority_shared_session_repairs_only_owed_delivery_without_resetting_pacing() {
    let (mut app, now, epoch) = delivered_fixture();
    let second = app.open_active_session_in_new_window_logical().unwrap();
    for _ in 0..4 {
        app.observe_title_drift(0, now);
    }
    let last = app.title_drift.last_flush[&0];
    app.windows
        .get_mut(&second)
        .unwrap()
        .tab_title_epochs
        .remove(&0);
    let titles = app.tab_titles(second);
    assert!(titles.iter().any(|title| title == "delivered"));
    for wid in [WindowId(0), second] {
        assert_eq!(app.windows[&wid].tab_title_epochs.get(&0), Some(&epoch));
    }
    assert_eq!(app.title_drift.last_flush[&0], last);
}

#[test]
fn title_authority_cross_window_swap_rebinds_both_outer_guards() {
    let (mut app, _, _) = delivered_fixture();
    let second = app.open_active_session_in_new_window_logical().unwrap();
    let _ = app.tab_titles(second);
    {
        let mut windows = app.windows.iter_mut();
        let (_, mut a) = windows.next().unwrap();
        let (_, mut b) = windows.next().unwrap();
        std::mem::swap(&mut a.tab_set, &mut b.tab_set);
        std::mem::swap(&mut a.tab_title_epochs, &mut b.tab_title_epochs);
    }
    let _ = app.tab_titles(second);
    let _ = app.windows.take_title_pending();
    let revision = app.windows.title_authority().revision();
    let _ = app
        .windows
        .get_mut(&WindowId(0))
        .unwrap()
        .tab_set
        .active_mut();
    assert!(!revision.is_current(app.windows.title_authority()));
    assert_eq!(
        app.windows
            .take_title_pending()
            .into_iter()
            .collect::<Vec<_>>(),
        [0]
    );
    let revision = app.windows.title_authority().revision();
    let _ = app.windows.get_mut(&second).unwrap().tab_set.active_mut();
    assert!(!revision.is_current(app.windows.title_authority()));
    assert_eq!(
        app.windows
            .take_title_pending()
            .into_iter()
            .collect::<Vec<_>>(),
        [second.0]
    );
}

#[test]
fn title_authority_contended_join_is_owed_and_recovers_at_publication_without_output() {
    let (mut app, hidden, sid, _, epoch) = split_fixture();
    let term = app.pool.get(sid).unwrap().term.clone();
    let held = term.lock().unwrap();
    app.windows
        .get_mut(&WindowId(0))
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(hidden);
    let _ = app.tab_titles(WindowId(0));
    assert!(
        !app.windows[&WindowId(0)]
            .tab_title_epochs
            .contains_key(&sid)
    );
    assert!(app.title_drift.pending.contains(&sid));
    drop(held);
    let _ = app.tab_titles(WindowId(0));
    assert_eq!(
        app.windows[&WindowId(0)].tab_title_epochs.get(&sid),
        Some(&epoch)
    );
    assert!(
        app.title_drift.pending.contains(&sid),
        "publication cannot erase the separate ordinary retry"
    );
}

#[test]
fn title_authority_same_id_whole_window_replacement_delivers_before_inspection() {
    let (mut app, _, epoch) = delivered_fixture();
    // Deliberately construct a fresh logical window with the same IDs and shared terminal
    // capability. The copied topology/geometry describes the replacement fixture, not a loan.
    let replacement = {
        let ws = &app.windows[&WindowId(0)];
        let terminal = ws.active_terminal.as_ref().unwrap();
        Box::new(WindowState::new_terminal(
            terminal.term.clone(),
            terminal.master,
            terminal.sink.clone(),
            terminal.ui_waiting.clone(),
            terminal.session,
            ws.rows,
            ws.cols,
            ws.metrics,
            ws.tabs.clone(),
            ws.layouts.clone(),
            ws.tab_set.clone(),
        ))
    };
    let mut retired = {
        let mut ws = app.windows.get_mut(&WindowId(0)).unwrap();
        std::mem::replace(&mut *ws, replacement)
    };
    assert!(!app.windows[&WindowId(0)].tab_title_epochs.contains_key(&0));
    let _ = app
        .inspect_app(crate::app_control::InspectRequest::Tabs)
        .unwrap();
    assert_eq!(
        app.windows[&WindowId(0)].tab_title_epochs.get(&0),
        Some(&epoch)
    );
    assert!(
        app.windows[&WindowId(0)]
            .tab_chrome_titles
            .values()
            .any(|title| title == "delivered")
    );
    let revision = app.windows.title_authority().revision();
    let _ = retired.tab_set.active_mut();
    retired.tab_title_epochs.remove(&0);
    assert!(revision.is_current(app.windows.title_authority()));
}

#[test]
fn title_authority_chrome_contention_reconciles_at_inspection_without_an_output_wake() {
    use crate::session_log::clause_tests::{clause_app, open_step, open_turn};
    let (mut app, timeline) = clause_app(true);
    let wid = WindowId(0);
    app.pool
        .get(0)
        .unwrap()
        .term
        .lock()
        .unwrap()
        .process(b"\x1b]2;chrome delivery\x07");
    let turn = open_turn(&timeline);
    let step = open_step(&timeline, turn, "shell");
    let now = Instant::now();
    for _ in 0..4 {
        app.observe_title_drift(0, now);
    }
    let epoch = app.title_drift.seen[&0];
    let paced = app.title_drift.last_flush[&0];
    let cached = app.session_chrome[&0]
        .clause
        .clone()
        .zip(app.session_chrome[&0].clause_line.clone());
    assert!(cached.is_some());
    {
        let mut log = timeline.lock().unwrap();
        let at = log.slog().now();
        log.slog_mut()
            .log_mut_for_journal()
            .close(
                step,
                aterm_slog::Close::new(
                    aterm_slog::Out::Step(aterm_slog::StepOut::Ok),
                    aterm_slog::Src::Transcript,
                    aterm_slog::Conf::Exact,
                ),
                at,
            )
            .unwrap();
    }
    open_step(&timeline, turn, "edit");
    {
        let _busy = timeline.lock().unwrap();
        assert_eq!(
            app.chrome_clause_read(wid, 0, &timeline, crate::turn_ledger::now_ms()),
            cached
        );
        assert!(app.session_chrome_retry.contains(&0));
        assert!(app.title_drift.pending.contains(&0));
        assert!(!app.windows[&wid].tab_title_epochs.contains_key(&0));
    }
    // No output or drift flush: the canonical read publishes the owed current chrome.
    app.inspect_app(crate::app_control::InspectRequest::Tabs)
        .unwrap();
    assert_eq!(app.windows[&wid].tab_title_epochs.get(&0), Some(&epoch));
    assert!(
        app.session_chrome[&0]
            .ext
            .tooltip
            .as_deref()
            .is_some_and(|text| text.contains("now: editing"))
    );
    assert_eq!(app.title_drift.last_flush[&0], paced);
    assert!(
        app.title_drift.pending.contains(&0),
        "publication keeps the separate retry obligation"
    );
    assert!(
        !app.title_drift.completed[&0]
            .authority
            .is_current(app.windows.title_authority())
    );
    assert!(app.flush_title_drift(0, now + DRIFT_REFRESH_MIN_INTERVAL));
    assert!(!app.title_drift.pending.contains(&0));
    assert!(!app.session_chrome_retry.contains(&0));
    assert!(
        app.title_drift.completed[&0]
            .authority
            .is_current(app.windows.title_authority())
    );
}

#[test]
fn title_authority_retained_live_observers_and_extra_custody_cannot_abandon_the_canonical_owner() {
    let run = |fault_at: Option<usize>| {
        let (mut app, hidden, sid, now, epoch) = split_fixture();
        let wid = WindowId(0);
        let original = app.windows[&wid].tab_set.active().unwrap().focus;
        let retained = app.windows[&wid].tab_set.title_owner();
        let fresh = tab_model::TitleAuthority::default();
        let mut bind = || {
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .bind_title_owner(wid.0, &fresh)
        };
        let old_extra = if fault_at == Some(1) {
            tab_model::with_title_binding_replacement_fault(bind)
        } else {
            bind()
        };
        assert!(retained.same_identity(&app.windows[&wid].tab_set.title_owner()));
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(hidden);
        app.inspect_app(crate::app_control::InspectRequest::Tabs)
            .unwrap();
        assert_eq!(app.windows[&wid].tab_title_epochs.get(&sid), Some(&epoch));
        assert!(
            app.windows[&wid]
                .tab_chrome_titles
                .values()
                .any(|title| title == "joined immediately")
        );

        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(original);
        app.inspect_app(crate::app_control::InspectRequest::Tabs)
            .unwrap();
        for _ in 0..4 {
            app.observe_title_drift(sid, now);
        }
        assert!(
            app.title_drift.completed[&sid]
                .authority
                .is_current(app.windows.title_authority())
        );
        assert!(!app.windows[&wid].tab_title_epochs.contains_key(&sid));
        let mut bind = || {
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .bind_title_owner(wid.0, &fresh)
        };
        let _new_extra = if fault_at == Some(2) {
            tab_model::with_title_binding_replacement_fault(bind)
        } else {
            bind()
        };
        drop(old_extra);
        assert!(retained.same_identity(&app.windows[&wid].tab_set.title_owner()));
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(hidden);
        app.inspect_app(crate::app_control::InspectRequest::Tabs)
            .unwrap();
        assert_eq!(app.windows[&wid].tab_title_epochs.get(&sid), Some(&epoch));
        assert!(
            app.windows[&wid]
                .tab_chrome_titles
                .values()
                .any(|title| title == "joined immediately")
        );
        app.on_output_wake(sid, now);
        assert_eq!(app.windows[&wid].tab_title_epochs.get(&sid), Some(&epoch));
    };
    run(None);
    for stage in [1, 2] {
        assert!(std::panic::catch_unwind(|| run(Some(stage))).is_err());
    }
}

#[test]
fn title_authority_reattached_populated_receipt_is_replaced_before_first_publication() {
    let run = |keep_old_evidence: bool| {
        let (mut app, now, epoch1) = delivered_fixture();
        let source = WindowId(0);
        let alias = app.open_active_session_in_new_window_logical().unwrap();
        app.split_active_stub_tab(alias);
        for _ in 0..4 {
            app.observe_title_drift(0, now);
        }
        assert!(!tabs_label_session(
            &app.windows[&alias].tab_set,
            &app.view_store,
            0
        ));
        let retained = app.windows.remove(&source).unwrap();
        assert_eq!(
            retained.tab_title_epochs.get(&0),
            Some(&epoch1),
            "retain populated E1 receipt"
        );
        assert!(
            app.pool.get(0).is_some(),
            "nonlabeling split alias keeps the session alive"
        );
        app.pool
            .get(0)
            .unwrap()
            .term
            .lock()
            .unwrap()
            .process(b"\x1b]2;retained root epoch two\x07");
        let observed = now + DRIFT_REFRESH_MIN_INTERVAL + Duration::from_millis(1);
        app.observe_title_drift(0, observed);
        let epoch2 = app.title_drift.seen[&0];
        assert_ne!(epoch1, epoch2);
        assert_eq!(app.title_drift.last_flush[&0], observed);
        assert!(
            app.title_drift.completed[&0]
                .authority
                .is_current(app.windows.title_authority())
        );
        assert!(!app.windows[&alias].tab_title_epochs.contains_key(&0));
        let attach = || {
            app.windows.insert(source, retained);
            // No more output: the first canonical publication must acknowledge E2, not merely
            // display a fresh string while retaining the E1 delivery receipt.
            app.inspect_app(crate::app_control::InspectRequest::Tabs)
                .unwrap();
            assert_eq!(app.windows[&source].tab_title_epochs.get(&0), Some(&epoch2));
            assert_eq!(app.title_drift.last_flush[&0], observed);
            assert!(!app.title_drift.pending.contains(&0));
            assert!(
                app.windows[&source]
                    .tab_chrome_titles
                    .values()
                    .any(|title| title == "retained root epoch two")
            );
        };
        if keep_old_evidence {
            with_fault(Fault::IncomingEvidence, attach);
        } else {
            attach();
        }
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}

#[test]
fn title_authority_retry_created_during_successful_delivery_publication_cannot_be_certified() {
    use crate::session_log::clause_tests::{clause_app, open_step, open_turn};
    let run = |mint_pending: bool| {
        let (mut app, timeline) = clause_app(true);
        let turn = open_turn(&timeline);
        let _step = open_step(&timeline, turn, "shell");
        app.pool
            .get(0)
            .unwrap()
            .term
            .lock()
            .unwrap()
            .process(b"\x1b]2;before publication\x07");
        let now = Instant::now();
        for _ in 0..4 {
            app.observe_title_drift(0, now);
        }
        assert!(
            app.title_drift.completed[&0]
                .authority
                .is_current(app.windows.title_authority())
        );
        app.pool
            .get(0)
            .unwrap()
            .term
            .lock()
            .unwrap()
            .process(b"\x1b]2;after delivery\x07");
        let epoch = app.pool.get(0).unwrap().term.lock().unwrap().title_epoch();
        let hook = Box::new(move |app: &mut App| {
            assert_eq!(
                app.title_drift.seen.get(&0),
                Some(&epoch),
                "resolution already succeeded"
            );
            assert_eq!(
                app.windows[&WindowId(0)].tab_title_epochs.get(&0),
                Some(&epoch)
            );
            assert!(!app.title_drift.pending.contains(&0));
            let _busy = timeline.lock().unwrap();
            let _ = app.chrome_clause_read(WindowId(0), 0, &timeline, crate::turn_ledger::now_ms());
            assert!(
                app.title_drift.pending.contains(&0),
                "real chrome contention creates a fresh obligation during publication"
            );
            assert!(app.session_chrome_retry.contains(&0));
        });
        crate::title_authority::with_before_publication(hook, || {
            let mut publish = || {
                assert!(app.flush_title_drift(0, now + DRIFT_REFRESH_MIN_INTERVAL));
                assert!(app.title_drift.pending.contains(&0));
                assert!(app.session_chrome_retry.contains(&0));
                assert!(
                    !app.title_drift.completed[&0]
                        .authority
                        .is_current(app.windows.title_authority()),
                    "successful resolution must not certify a retry created during its publication"
                );
            };
            if mint_pending {
                with_fault(Fault::MintPending, publish);
            } else {
                publish();
            }
        });
        assert!(app.flush_title_drift(0, now + DRIFT_REFRESH_MIN_INTERVAL * 2));
        assert!(!app.title_drift.pending.contains(&0));
        assert!(!app.session_chrome_retry.contains(&0));
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}
