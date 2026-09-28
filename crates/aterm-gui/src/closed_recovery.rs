// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, identity-free recovery records for closed views and tabs.
//!
//! These ledgers are deliberately separate. `Cmd-Shift-T` consumes only [`ClosedTab`]
//! records; the named Reopen Closed View command consumes only [`ClosedView`] records.
//! A candidate is removed only by an explicit token commit after reconstruction succeeds,
//! so an unavailable document/app never destroys the user's last recovery path.

use std::collections::VecDeque;

use crate::WindowId;
use crate::restore::{RestoreBranch, RestoredTab, RestoredView, SplitKind};
use crate::tab_model::TabId;

pub(crate) const CLOSED_VIEW_LIMIT: usize = 64;
pub(crate) const CLOSED_TAB_LIMIT: usize = 32;
pub(crate) const CLOSED_VIEW_MAX_AGE_MS: u64 = 30 * 60 * 1_000;
pub(crate) const CLOSED_TAB_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1_000;

/// Where a removed leaf belonged beneath its parent split. Reopening uses the original
/// parent path when the tab is still live; otherwise the view becomes a one-leaf tab.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ClosedViewPlacement {
    pub(crate) parent_path: Vec<RestoreBranch>,
    pub(crate) removed_branch: RestoreBranch,
    pub(crate) axis: SplitKind,
    pub(crate) ratio: f32,
}

impl ClosedViewPlacement {
    pub(crate) fn new(
        parent_path: Vec<RestoreBranch>,
        removed_branch: RestoreBranch,
        axis: SplitKind,
        ratio: f32,
    ) -> Option<Self> {
        (parent_path.len() <= 32 && ratio.is_finite()).then_some(Self {
            parent_path,
            removed_branch,
            axis,
            ratio: ratio.clamp(0.05, 0.95),
        })
    }
}

/// One non-last leaf close. It carries no retired `ViewId`, app instance id, or terminal
/// pool identity; reconstruction must mint a new view identity.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ClosedView {
    pub(crate) original_window: WindowId,
    pub(crate) original_tab: TabId,
    pub(crate) view: RestoredView,
    pub(crate) placement: ClosedViewPlacement,
}

/// One whole-tab close, including its recursive split tree and presentation position.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ClosedTab {
    pub(crate) original_window: WindowId,
    pub(crate) original_index: usize,
    pub(crate) tab: RestoredTab,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LeafCloseRecordKind {
    ClosedView,
    ClosedTab,
}

/// Closing the only leaf is always a tab close. The caller uses this before mutating the
/// tree, which prevents one gesture from entering both recovery ledgers.
pub(crate) const fn leaf_close_record_kind(leaves_before_close: usize) -> LeafCloseRecordKind {
    if leaves_before_close <= 1 {
        LeafCloseRecordKind::ClosedTab
    } else {
        LeafCloseRecordKind::ClosedView
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RecoveryToken(u64);

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ReopenCandidate<T> {
    pub(crate) token: RecoveryToken,
    pub(crate) value: T,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RecoveryCommitError {
    StaleCandidate,
}

#[derive(Clone, PartialEq, Debug)]
struct RecoveryEntry<T> {
    sequence: u64,
    closed_at_ms: u64,
    value: T,
}

/// A deterministic newest-last, drop-oldest ledger. Time is supplied by the host so tests
/// and replay never depend on wall-clock reads inside the state machine.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RecoveryLedger<T> {
    entries: VecDeque<RecoveryEntry<T>>,
    capacity: usize,
    max_age_ms: u64,
    next_sequence: u64,
}

impl<T> RecoveryLedger<T> {
    pub(crate) fn new(capacity: usize, max_age_ms: u64) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            max_age_ms,
            next_sequence: 1,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn oldest(&self, now_ms: u64) -> Option<&T> {
        self.entries.front().and_then(|entry| {
            (now_ms.saturating_sub(entry.closed_at_ms) <= self.max_age_ms).then_some(&entry.value)
        })
    }

    pub(crate) fn push(&mut self, value: T, now_ms: u64) {
        self.prune(now_ms);
        if self.capacity == 0 {
            return;
        }
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.entries.push_back(RecoveryEntry {
            sequence,
            closed_at_ms: now_ms,
            value,
        });
    }

    pub(crate) fn prune(&mut self, now_ms: u64) {
        while self
            .entries
            .front()
            .is_some_and(|entry| now_ms.saturating_sub(entry.closed_at_ms) > self.max_age_ms)
        {
            self.entries.pop_front();
        }
    }
}

impl<T: Clone> RecoveryLedger<T> {
    pub(crate) fn candidate_snapshot(&self, now_ms: u64) -> Option<ReopenCandidate<T>> {
        let entry = self.entries.back()?;
        (now_ms.saturating_sub(entry.closed_at_ms) <= self.max_age_ms).then(|| ReopenCandidate {
            token: RecoveryToken(entry.sequence),
            value: entry.value.clone(),
        })
    }

    /// Copy the latest candidate without consuming it. Reconstruction may fail freely.
    pub(crate) fn candidate(&mut self, now_ms: u64) -> Option<ReopenCandidate<T>> {
        self.prune(now_ms);
        self.candidate_snapshot(now_ms)
    }

    /// Consume exactly the candidate that was successfully reconstructed. Any intervening
    /// push/prune makes the token stale and leaves the current newest record untouched.
    pub(crate) fn commit(
        &mut self,
        candidate: RecoveryToken,
        now_ms: u64,
    ) -> Result<T, RecoveryCommitError> {
        self.prune(now_ms);
        if self.entries.back().map(|entry| entry.sequence) != Some(candidate.0) {
            return Err(RecoveryCommitError::StaleCandidate);
        }
        self.entries
            .pop_back()
            .map(|entry| entry.value)
            .ok_or(RecoveryCommitError::StaleCandidate)
    }
}

/// Independent ledgers with independent count and age bounds.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ClosedRecoveryLedgers {
    pub(crate) views: RecoveryLedger<ClosedView>,
    pub(crate) tabs: RecoveryLedger<ClosedTab>,
}

impl Default for ClosedRecoveryLedgers {
    fn default() -> Self {
        Self {
            views: RecoveryLedger::new(CLOSED_VIEW_LIMIT, CLOSED_VIEW_MAX_AGE_MS),
            tabs: RecoveryLedger::new(CLOSED_TAB_LIMIT, CLOSED_TAB_MAX_AGE_MS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restore::{NativeLeafRestore, RestoredSplitTree, TerminalLeafRestore};

    fn terminal(label: &str) -> RestoredView {
        RestoredView::Terminal(TerminalLeafRestore {
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
            identity: None,
            agent: None,
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
    fn view_and_tab_ledgers_have_separate_bounds_and_only_leaf_rule() {
        let mut ledgers = ClosedRecoveryLedgers {
            views: RecoveryLedger::new(2, 100),
            tabs: RecoveryLedger::new(3, 100),
        };
        let placement =
            ClosedViewPlacement::new(Vec::new(), RestoreBranch::Second, SplitKind::Vertical, 0.7)
                .unwrap();
        for index in 0..4 {
            ledgers.views.push(
                ClosedView {
                    original_window: WindowId(1),
                    original_tab: TabId::from_stored(9),
                    view: terminal(&format!("view-{index}")),
                    placement: placement.clone(),
                },
                index,
            );
            ledgers.tabs.push(tab(&format!("tab-{index}")), index);
        }
        assert_eq!(ledgers.views.len(), 2);
        assert_eq!(ledgers.tabs.len(), 3);
        assert_eq!(leaf_close_record_kind(1), LeafCloseRecordKind::ClosedTab);
        assert_eq!(leaf_close_record_kind(2), LeafCloseRecordKind::ClosedView);
    }

    #[test]
    fn unavailable_native_descriptor_is_valid_recovery_data_not_code() {
        let unavailable = RestoredView::Native(NativeLeafRestore {
            restore_tag: "future.canvas".to_string(),
            route: None,
            uri: None,
            config_editor: false,
            source_anchor: 0,
            selection: None,
            editor_selections: Vec::new(),
            primary_selection: 0,
            viewport_anchor: 0,
            durable_seq: 0,
            metadata: "command=rm -rf /".to_string(),
        });
        let mut ledger = RecoveryLedger::new(1, 100);
        ledger.push(unavailable.clone(), 0);
        assert_eq!(ledger.candidate(0).unwrap().value, unavailable);
    }

    /// Tier-1 for `ClosedRecoveryLedgers`, driven through the App's shipping close
    /// and reopen paths: `close_active_tab` on a two-leaf tab, `close_tab_at` on a
    /// one-leaf tab, `reopen_last_closed_view` and `reopen_last_closed_tab`. The
    /// App's ledgers are rebuilt with the model's capacities (2 views, 3 tabs), so
    /// the real `push` saturates where the model's clamp does. Which ledger each
    /// close records into is the App's choice, never the test's.
    #[test]
    fn dual_ledgers_tier1_conform_and_reject_double_record_negative_control() {
        use aterm_spec::derive::closed_recovery_ledgers_model;
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
            // Tab 0 is the bootstrap terminal; the model's one tab is the one after it.
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
        // The view ledger is full: the real push drops the oldest record.
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

        // Failed reconstruction consumes neither ledger. With no window left
        // to host it, both reopens fail and both candidates stay.
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
        // Healthy, the two failures are one transition (they differ only in the
        // ledger a consuming failure would hit), so `admits` names `FailView`.
        assert_eq!(
            model.successors("FailTab", &after_view_failure).as_slice(),
            std::slice::from_ref(&after_tab_failure),
            "the failed tab reopen conforms to FailTab"
        );

        // Negative controls. One close recorded in both ledgers:
        let mut double_record = after_view.clone();
        double_record.insert("tab_ledger", 1);
        double_record.insert("double_recorded", 1);
        assert_eq!(admits(&model, &before_view, &double_record), None);
        assert!(!model.check_invariant("OnlyOneRecordPerClose", &double_record));
        // A full push that evicts one too few, in either ledger:
        let mut overfull_view = at_view_cap.clone();
        overfull_view.insert("view_ledger", 3);
        assert_eq!(admits(&model, &before_full_view, &overfull_view), None);
        assert!(!model.check_invariant("ViewLedgerBounded", &overfull_view));
        let mut overfull_tab = at_tab_cap.clone();
        overfull_tab.insert("tab_ledger", 4);
        assert_eq!(admits(&model, &before_full_tab, &overfull_tab), None);
        assert!(!model.check_invariant("TabLedgerBounded", &overfull_tab));
        // A failed reopen that consumed its record:
        let mut lossy = after_view_failure;
        lossy.insert("view_ledger", reopened_tab["view_ledger"] - 1);
        lossy.insert("lost_on_failure", 1);
        assert_eq!(admits(&model, &reopened_tab, &lossy), None);
        assert!(!model.check_invariant("FailedReopenRetainsRecord", &lossy));
    }
}
