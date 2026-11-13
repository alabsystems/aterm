// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::restore::{RestoredSplitTree, TerminalLeafRestore};

fn terminal(label: &str) -> RestoredView {
    RestoredView::Terminal(TerminalLeafRestore {
        sid: None,
        lineage: None,
        log_ep: None,
        cwd: Some(format!("/{label}")),
        title: label.to_string(),
        profile: None,
        local_id: None,
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        autopilot: None,
        land_at: None,
        autopilot_by: None,
        theme: None,
        agent: None,
        supervised: None,
        lost: None,
        held: false,
        holds: Vec::new(),
        claim_ended: false,
        not_claim_worker: false,
        claim_worker: None,
        task: None,
    })
}

fn tab(label: &str) -> ClosedTab {
    ClosedTab {
        original_window: WindowId(1),
        original_index: 0,
        tab: RestoredTab {
            root: RestoredSplitTree::leaf(terminal(label)),
            focused_path: Vec::new(),
            zoomed: false,
            parked: false,
            ephemeral: false,
            fleet: false,
        },
    }
}

#[test]
fn count_bound_drops_oldest_and_failed_reopen_consumes_nothing() {
    let mut ledger = RecoveryLedger::new(3, 1_000);
    for (now, label) in ["one", "two", "three", "four"].into_iter().enumerate() {
        ledger.push(tab(label), now as u64);
    }
    assert_eq!(ledger.len(), 3);
    let failed = ledger.candidate(10).expect("latest");
    assert_eq!(failed.value.tab.root, tab("four").tab.root);
    assert_eq!(ledger.len(), 3, "inspection/failure never consumes");
    let consumed = ledger.commit(failed.token, 10).unwrap();
    assert_eq!(consumed.tab.root, tab("four").tab.root);
    assert_eq!(ledger.len(), 2);
    let oldest = ledger.entries.front().unwrap();
    assert_eq!(oldest.value.tab.root, tab("two").tab.root);
}

#[test]
fn has_candidate_agrees_with_the_snapshot() {
    let mut ledger = RecoveryLedger::new(4, 10);
    for now in [0, 5, 20] {
        assert_eq!(
            ledger.has_candidate(now),
            ledger.candidate_snapshot(now).is_some()
        );
        assert!(!ledger.has_candidate(now), "empty");
    }
    ledger.push(tab("one"), 5);
    for now in [5, 15, 16, 40] {
        assert_eq!(
            ledger.has_candidate(now),
            ledger.candidate_snapshot(now).is_some(),
            "at {now}"
        );
    }
    assert!(ledger.has_candidate(15) && !ledger.has_candidate(16));
}

#[test]
fn age_expiry_and_candidate_tokens_are_deterministic() {
    let mut ledger = RecoveryLedger::new(4, 10);
    ledger.push(tab("old"), 5);
    let stale = ledger.candidate(10).unwrap();
    ledger.push(tab("new"), 11);
    assert_eq!(
        ledger.commit(stale.token, 11),
        Err(RecoveryCommitError::StaleCandidate)
    );
    assert_eq!(ledger.len(), 2);
    ledger.prune(16);
    assert_eq!(ledger.len(), 1, "age is measured from each close");
    assert_eq!(
        ledger.candidate(16).unwrap().value.tab.root,
        tab("new").tab.root
    );
}

#[test]
fn dual_ledgers_tier1_conform_and_reject_double_record_negative_control() {
    use aterm_models_native::closed_recovery_ledgers_model;
    use aterm_spec::interp::{State, admits};

    use crate::App;
    use crate::native_settings::SettingsRoute;
    use crate::tab_model::SplitAxis;

    #[derive(Clone, Copy, Default)]
    struct Facts {
        failures: i64,
    }

    fn project(model: &aterm_spec::derive::Model, app: &App, facts: Facts) -> State {
        let window = &app.windows[&WindowId(0)];
        let live_tabs = window.tab_set.len() as i64 - 1;
        let live_leaves = if live_tabs == 1 {
            window.tab_set.tabs()[1].root.len() as i64
        } else {
            0
        };
        let mut state = model.init_state();
        state.insert("view_ledger", app.closed_recovery.views.len() as i64);
        state.insert("tab_ledger", app.closed_recovery.tabs.len() as i64);
        state.insert("live_tabs", live_tabs);
        state.insert("live_leaves", live_leaves);
        state.insert("failures", facts.failures);
        state
    }

    fn assert_step(
        model: &aterm_spec::derive::Model,
        before: &State,
        after: &State,
        action: &'static str,
    ) {
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "shipping recovery transition must conform specifically to {action}"
        );
        assert_eq!(admits(model, before, after), Some(action));
    }

    let model = closed_recovery_ledgers_model();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let facts = Facts::default();
    app.closed_recovery = ClosedRecoveryLedgers {
        views: RecoveryLedger::new(2, CLOSED_VIEW_MAX_AGE_MS),
        tabs: RecoveryLedger::new(3, CLOSED_TAB_MAX_AGE_MS),
    };
    let open_two_leaf_tab = |app: &mut App| {
        assert!(app.open_settings_tab(SettingsRoute::About));
        app.split_active_with_stub_terminal(wid, SplitAxis::Vertical);
    };
    open_two_leaf_tab(&mut app);
    assert_eq!(project(&model, &app, facts), model.init_state());

    let step = |app: &mut App, action: &'static str, operate: &dyn Fn(&mut App)| {
        let before = project(&model, app, facts);
        operate(app);
        let after = project(&model, app, facts);
        assert_step(&model, &before, &after, action);
        assert!(app.structural_invariants_ok());
        (before, after)
    };
    let close_leaf = |app: &mut App| {
        app.close_active_tab();
    };
    let close_tab = |app: &mut App| {
        assert!(!app.close_tab_at(WindowId(0), 1));
    };

    let (before_view, after_view) = step(&mut app, "CloseView", &close_leaf);
    step(&mut app, "ReopenView", &|app| {
        app.reopen_last_closed_view().unwrap();
    });
    step(&mut app, "CloseView", &close_leaf);
    step(&mut app, "CloseTab", &close_tab);
    step(&mut app, "OpenTab", &open_two_leaf_tab);
    step(&mut app, "CloseView", &close_leaf);
    step(&mut app, "CloseTab", &close_tab);
    step(&mut app, "OpenTab", &open_two_leaf_tab);
    let (before_full_view, at_view_cap) = step(&mut app, "CloseView", &close_leaf);
    assert_eq!(at_view_cap["view_ledger"], 2);
    step(&mut app, "CloseTab", &close_tab);
    step(&mut app, "OpenTab", &open_two_leaf_tab);
    step(&mut app, "CloseView", &close_leaf);
    let (before_full_tab, at_tab_cap) = step(&mut app, "CloseTab", &close_tab);
    assert_eq!(at_tab_cap["tab_ledger"], 3);
    let (_, reopened_tab) = step(&mut app, "ReopenTab", &|app| {
        app.reopen_last_closed_tab().unwrap();
    });

    let views = app.closed_recovery.views.len();
    let tabs = app.closed_recovery.tabs.len();
    app.windows.clear();
    app.frontmost_window = None;
    assert!(app.reopen_last_closed_view().is_err());
    assert_eq!(app.closed_recovery.views.len(), views);
    let mut after_view_failure = reopened_tab.clone();
    after_view_failure.insert("failures", 1);
    assert_step(&model, &reopened_tab, &after_view_failure, "FailView");
    assert!(app.reopen_last_closed_tab().is_err());
    assert_eq!(app.closed_recovery.tabs.len(), tabs);
    let mut after_tab_failure = after_view_failure.clone();
    after_tab_failure.insert("failures", 2);
    assert_eq!(
        model.successors("FailTab", &after_view_failure).as_slice(),
        std::slice::from_ref(&after_tab_failure),
        "the failed tab reopen conforms to FailTab"
    );

    let mut double_record = after_view.clone();
    double_record.insert("tab_ledger", 1);
    double_record.insert("double_recorded", 1);
    assert_eq!(admits(&model, &before_view, &double_record), None);
    assert!(!model.check_invariant("OnlyOneRecordPerClose", &double_record));
    let mut overfull_view = at_view_cap.clone();
    overfull_view.insert("view_ledger", 3);
    assert_eq!(admits(&model, &before_full_view, &overfull_view), None);
    assert!(!model.check_invariant("ViewLedgerBounded", &overfull_view));
    let mut overfull_tab = at_tab_cap.clone();
    overfull_tab.insert("tab_ledger", 4);
    assert_eq!(admits(&model, &before_full_tab, &overfull_tab), None);
    assert!(!model.check_invariant("TabLedgerBounded", &overfull_tab));
    let mut lossy = after_view_failure;
    lossy.insert("view_ledger", reopened_tab["view_ledger"] - 1);
    lossy.insert("lost_on_failure", 1);
    assert_eq!(admits(&model, &reopened_tab, &lossy), None);
    assert!(!model.check_invariant("FailedReopenRetainsRecord", &lossy));
}
