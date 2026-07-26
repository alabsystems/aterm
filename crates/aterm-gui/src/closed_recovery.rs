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

    #[test]
    fn dual_ledgers_tier1_conform_and_reject_double_record_negative_control() {
        use aterm_spec::derive::closed_recovery_ledgers_model;
        use aterm_spec::interp::{State, admits};

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
        let initial = model.init_state();
        let placement =
            ClosedViewPlacement::new(Vec::new(), RestoreBranch::Second, SplitKind::Vertical, 0.5)
                .unwrap();
        let mut ledgers = ClosedRecoveryLedgers {
            views: RecoveryLedger::new(2, 100),
            tabs: RecoveryLedger::new(3, 100),
        };
        ledgers.views.push(
            ClosedView {
                original_window: WindowId(1),
                original_tab: TabId::from_stored(7),
                view: terminal("closed-view"),
                placement,
            },
            1,
        );
        let mut after_view = initial.clone();
        after_view.insert("view_ledger", ledgers.views.len() as i64);
        after_view.insert("live_leaves", 1);
        assert_step(&model, &initial, &after_view, "CloseView");

        let before_failure_len = ledgers.views.len();
        let _failed_candidate = ledgers.views.candidate(2).unwrap();
        assert_eq!(ledgers.views.len(), before_failure_len);
        let mut after_failure = after_view.clone();
        after_failure.insert("failures", 1);
        assert_step(&model, &after_view, &after_failure, "FailView");

        let mut double_record = after_view.clone();
        double_record.insert("tab_ledger", 1);
        double_record.insert("double_recorded", 1);
        assert_eq!(admits(&model, &initial, &double_record), None);
        assert!(!model.check_invariant("OnlyOneRecordPerClose", &double_record));
    }
}
