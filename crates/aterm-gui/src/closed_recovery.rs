// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, identity-free recovery records for closed views and tabs.

use std::collections::VecDeque;

use crate::WindowId;
use crate::restore::{RestoreBranch, RestoredSplitTree, RestoredTab, RestoredView, SplitKind};
use crate::tab_model::TabId;

pub(crate) const CLOSED_VIEW_LIMIT: usize = 64;
pub(crate) const CLOSED_TAB_LIMIT: usize = 32;
pub(crate) const CLOSED_VIEW_MAX_AGE_MS: u64 = 30 * 60 * 1_000;
pub(crate) const CLOSED_TAB_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1_000;

/// Where a removed leaf belonged beneath its parent split.
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

/// One non-last leaf close.
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

/// Closing the only leaf is always a tab close.
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

/// A deterministic newest-last, drop-oldest ledger.
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

    fn live(&self, now_ms: u64) -> impl Iterator<Item = (&T, u64)> {
        self.entries
            .iter()
            .filter(move |entry| now_ms.saturating_sub(entry.closed_at_ms) <= self.max_age_ms)
            .map(|entry| {
                let expires_ms = entry
                    .closed_at_ms
                    .saturating_add(self.max_age_ms)
                    .saturating_add(1);
                (&entry.value, expires_ms)
            })
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
    /// Whether [`Self::candidate_snapshot`] would answer `Some`, without cloning the record.
    pub(crate) fn has_candidate(&self, now_ms: u64) -> bool {
        self.entries
            .back()
            .is_some_and(|entry| now_ms.saturating_sub(entry.closed_at_ms) <= self.max_age_ms)
    }

    pub(crate) fn candidate_snapshot(&self, now_ms: u64) -> Option<ReopenCandidate<T>> {
        let entry = self.entries.back()?;
        (now_ms.saturating_sub(entry.closed_at_ms) <= self.max_age_ms).then(|| ReopenCandidate {
            token: RecoveryToken(entry.sequence),
            value: entry.value.clone(),
        })
    }

    /// Copy the latest candidate without consuming it.
    pub(crate) fn candidate(&mut self, now_ms: u64) -> Option<ReopenCandidate<T>> {
        self.prune(now_ms);
        self.candidate_snapshot(now_ms)
    }

    /// Consume exactly the candidate that was successfully reconstructed.
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

impl ClosedRecoveryLedgers {
    /// Returns `(uri, expires_ms)` pairs for all recoverable document views at `now_ms`.
    pub(crate) fn document_uris(&self, now_ms: u64) -> Vec<(&str, u64)> {
        fn leaf_uri(view: &RestoredView) -> Option<&str> {
            match view {
                RestoredView::Native(native) => native.uri.as_deref(),
                RestoredView::Terminal(_) | RestoredView::Placeholder(_) => None,
            }
        }
        fn tree_uris<'a>(
            tree: &'a RestoredSplitTree,
            expires_ms: u64,
            uris: &mut Vec<(&'a str, u64)>,
        ) {
            match tree {
                RestoredSplitTree::Leaf { view } => {
                    uris.extend(leaf_uri(view).map(|uri| (uri, expires_ms)));
                }
                RestoredSplitTree::Split { first, second, .. } => {
                    tree_uris(first, expires_ms, uris);
                    tree_uris(second, expires_ms, uris);
                }
            }
        }
        let mut uris: Vec<(&str, u64)> = self
            .views
            .live(now_ms)
            .filter_map(|(closed, expires_ms)| leaf_uri(&closed.view).map(|uri| (uri, expires_ms)))
            .collect();
        for (closed, expires_ms) in self.tabs.live(now_ms) {
            tree_uris(&closed.tab.root, expires_ms, &mut uris);
        }
        uris
    }
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
#[path = "closed_recovery_tests.rs"]
mod tests;
