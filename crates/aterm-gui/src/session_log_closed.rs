// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-local index of recently closed sessions for `exits`, `log` and `brief`: each close's exit, linked in its roster record's store write, and its log and skeleton once attached.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aterm_session::{EdgeTable, LaunchNonce, Op, SessionId};
use aterm_slog::Log;

use crate::session_store::{ExitActor, ExitReason};

/// Maximum number of closed sessions retained in the index.
pub(crate) const CLOSED_INDEX_CAP: usize = 512;
/// Number of newest closed sessions that retain a skeleton log.
pub(crate) const CLOSED_SKELETONS: usize = 32;
/// Maximum encoded byte size of a closed session's skeleton log.
pub(crate) const CLOSED_SKELETON_BYTES: usize = 64 * 1024;

/// An edge row's estimated heap (its token, two session ids and a nonce in a
/// hash table's slot).
const EDGE_ROW_HEAP: usize = 128;

/// A close as the roster's `Exited` record says it: the facts an `exits` row
/// answers, linked in that record's store write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClosedExit {
    /// The roster seq of its `Exited` record: the `exits` row id.
    pub(crate) seq: u64,
    /// The close's process-clock reading (its session close's `mono`), on
    /// [`crate::turn_ledger::now_ms`]'s clock.
    pub(crate) t_ms: u64,
    /// The session it was.
    pub(crate) sid: SessionId,
    /// The process-local id it was routed by.
    pub(crate) local_id: u64,
    /// Why it closed.
    pub(crate) reason: ExitReason,
    /// The child's exit status, when the close recovered one.
    pub(crate) exit_code: Option<i32>,
    /// Who closed it, raw (a sid nobody minted included): held in RAM only.
    pub(crate) actor: ExitActor,
}

/// One close's slot: its exit, and its log once attached.
struct ClosedSlot {
    exit: ClosedExit,
    log: Option<ClosedLog>,
}

impl ClosedSlot {
    /// Its charge against the cap: the slot, and its log's heap.
    fn charge(&self) -> usize {
        std::mem::size_of::<ClosedSlot>() + self.log.as_ref().map_or(0, ClosedLog::heap)
    }
}

/// One closed session's log, as the index keeps it.
pub(crate) struct ClosedLog {
    sid: SessionId,
    summary: Arc<str>,
    skeleton: Option<Arc<Log>>,
    /// The read grants, shared with every reader and revoker that clones it
    /// out of the index (never locked under the store's lock).
    reads: Arc<Mutex<EdgeTable>>,
    /// How many rows `reads` held at the close, for the estimated charge.
    read_rows: usize,
    nonce: LaunchNonce,
}

/// Snapshot of a closed session cloned under the store's read lock.
#[derive(Clone)]
pub(crate) struct ClosedRead {
    sid: SessionId,
    source: ClosedSource,
    reads: Arc<Mutex<EdgeTable>>,
    nonce: LaunchNonce,
}

impl ClosedRead {
    /// Returns the closed session's ID.
    pub(crate) fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// Returns the log source for rendering.
    pub(crate) fn source(&self) -> ClosedSource {
        self.source.clone()
    }

    /// Returns whether `presented` is granted `ReadScreen` on this closed session.
    pub(crate) fn edge_may_read(&self, presented: &aterm_session::EdgeToken) -> bool {
        let reads = self.reads.lock().unwrap_or_else(|p| p.into_inner());
        aterm_session::decide_edge(&reads, presented, &self.sid, Op::ReadScreen, &self.nonce)
            .is_permitted()
    }
}

/// Log source backing a closed-session read.
#[derive(Clone)]
pub(crate) enum ClosedSource {
    /// Retained skeleton log.
    Skeleton(Arc<Log>),
    /// Compact summary text.
    Summary(Arc<str>),
}

impl ClosedSource {
    /// Materializes the [`Log`] to render from.
    pub(crate) fn into_log(self) -> Option<Arc<Log>> {
        match self {
            ClosedSource::Skeleton(log) => Some(log),
            ClosedSource::Summary(text) => Log::from_closed_summary(&text).ok().map(Arc::new),
        }
    }
}

impl std::fmt::Debug for ClosedLogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClosedLogs")
            .field("slots", &self.slots.len())
            .field("charge", &self.charge)
            .finish()
    }
}

#[cfg(test)]
thread_local! {
    /// Thread-local count of skeletons built via [`ClosedLog::kept_from`].
    pub(crate) static SKELETONS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Retention tier for a closing session's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosedKeep {
    /// Retain both the summary and the 64 KiB skeleton log.
    Skeleton,
    /// Retain only the summary.
    Summary,
}

impl ClosedKeep {
    /// Returns the retention tier for close `index` in a batch of `of` closes.
    pub(crate) fn in_batch(index: usize, of: usize) -> ClosedKeep {
        if of - index <= CLOSED_SKELETONS {
            ClosedKeep::Skeleton
        } else {
            ClosedKeep::Summary
        }
    }

    /// Plans the retention tier for each view detach in `closes`.
    pub(crate) fn plan(closes: &[bool]) -> Vec<Option<ClosedKeep>> {
        let of = closes.iter().filter(|c| **c).count();
        let mut index = 0;
        let mut plan = Vec::with_capacity(closes.len());
        for &closes in closes {
            plan.push(closes.then(|| ClosedKeep::in_batch(index, of)));
            index += usize::from(closes);
        }
        plan
    }
}

impl ClosedLog {
    /// Builds a [`ClosedLog`] from `log` at retirement.
    pub(crate) fn kept_from(
        log: &Log,
        sid: SessionId,
        reads: Arc<Mutex<EdgeTable>>,
        read_rows: usize,
        nonce: LaunchNonce,
        keep: ClosedKeep,
    ) -> ClosedLog {
        let summary: Arc<str> = log.closed_summary().into();
        let skeleton = (keep == ClosedKeep::Skeleton).then(|| {
            #[cfg(test)]
            SKELETONS_BUILT.with(|n| n.set(n.get() + 1));
            Arc::new(log.kept_within(CLOSED_SKELETON_BYTES))
        });
        ClosedLog {
            sid,
            summary,
            skeleton,
            read_rows,
            reads,
            nonce,
        }
    }

    /// Returns the session ID.
    #[cfg(test)]
    pub(crate) fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// Returns the [`ClosedSource`] for this entry.
    pub(crate) fn source(&self) -> ClosedSource {
        match &self.skeleton {
            Some(log) => ClosedSource::Skeleton(Arc::clone(log)),
            None => ClosedSource::Summary(Arc::clone(&self.summary)),
        }
    }

    /// Clones a [`ClosedRead`] handle under the store's read lock.
    pub(crate) fn read(&self) -> ClosedRead {
        ClosedRead {
            sid: self.sid.clone(),
            source: self.source(),
            reads: Arc::clone(&self.reads),
            nonce: self.nonce,
        }
    }

    /// Clones the shared edge-grant table handle.
    pub(crate) fn reads(&self) -> Arc<Mutex<EdgeTable>> {
        Arc::clone(&self.reads)
    }

    /// Returns the estimated heap beyond its own bytes (its slot's charge holds those).
    fn heap(&self) -> usize {
        self.summary.len()
            + self.skeleton.as_ref().map_or(0, |s| s.footprint())
            + self.read_rows * EDGE_ROW_HEAP
    }

    /// Its estimated heap with its own bytes.
    #[cfg(test)]
    pub(crate) fn footprint(&self) -> usize {
        std::mem::size_of::<ClosedLog>() + self.heap()
    }
}

/// The index: the closed sessions' slots, oldest (lowest roster seq) first.
#[derive(Default)]
pub(crate) struct ClosedLogs {
    slots: VecDeque<ClosedSlot>,
    /// Σ the slots' charges, kept as they come and go.
    charge: usize,
}

impl ClosedLogs {
    /// Links a close's `exit` as the newest slot, in the store write that minted its roster seq, and returns any evicted skeleton logs.
    pub(crate) fn link(&mut self, exit: ClosedExit) -> Vec<Arc<Log>> {
        let mut freed = Vec::new();
        let slot = ClosedSlot { exit, log: None };
        self.charge = self.charge.saturating_add(slot.charge());
        self.slots.push_back(slot);
        let n = self.slots.len();
        if n > CLOSED_SKELETONS
            && let Some(log) = self
                .slots
                .get_mut(n - 1 - CLOSED_SKELETONS)
                .and_then(|s| s.log.as_mut())
            && let Some(skeleton) = log.skeleton.take()
        {
            self.charge = self.charge.saturating_sub(skeleton.footprint());
            freed.push(skeleton);
        }
        while self.slots.len() > CLOSED_INDEX_CAP {
            let Some(old) = self.slots.pop_front() else {
                break;
            };
            self.charge = self.charge.saturating_sub(old.charge());
            freed.extend(old.log.and_then(|l| l.skeleton));
        }
        freed
    }

    /// Attach `log` to the slot of the close whose roster seq is `seq` (the
    /// rows it was a source of leave the other entries' grants in the
    /// close-time sweep, `connections::sweep_session_closed_in`, as they
    /// leave live tables). A skeleton for a slot no longer among the newest
    /// [`CLOSED_SKELETONS`] comes back at once, as does a slot's earlier log
    /// and a skeleton whose slot has gone: what this returns, for the caller
    /// to free once the store's lock is released.
    pub(crate) fn keep_closed(&mut self, seq: u64, mut log: ClosedLog) -> Vec<Arc<Log>> {
        let n = self.slots.len();
        let at = self.slots.partition_point(|s| s.exit.seq < seq);
        let Some(slot) = self.slots.get_mut(at).filter(|s| s.exit.seq == seq) else {
            return log.skeleton.into_iter().collect();
        };
        debug_assert_eq!(slot.exit.sid, log.sid, "a log attached to another's slot");
        let mut freed = Vec::new();
        if n - at > CLOSED_SKELETONS {
            freed.extend(log.skeleton.take());
        }
        self.charge = self.charge.saturating_add(log.heap());
        if let Some(old) = slot.log.replace(log) {
            self.charge = self.charge.saturating_sub(old.heap());
            freed.extend(old.skeleton);
        }
        freed
    }

    /// The newest close of local id `local`, once its log is attached.
    pub(crate) fn by_local(&self, local: u64) -> Option<&ClosedLog> {
        self.slots
            .iter()
            .rev()
            .find(|s| s.exit.local_id == local)?
            .log
            .as_ref()
    }

    /// The newest close of session `sid`, once its log is attached.
    pub(crate) fn by_sid(&self, sid: &SessionId) -> Option<&ClosedLog> {
        self.slots
            .iter()
            .rev()
            .find(|s| s.exit.sid == *sid)?
            .log
            .as_ref()
    }

    /// The exits linked after roster seq `since`, oldest first (`control_session::cmd_exits`).
    pub(crate) fn exits_since(
        &self,
        since: u64,
    ) -> impl DoubleEndedIterator<Item = &ClosedExit> + ExactSizeIterator {
        let from = self.slots.partition_point(|s| s.exit.seq <= since);
        self.slots.range(from..).map(|s| &s.exit)
    }

    /// Returns cloned edge-grant tables for all attached entries.
    pub(crate) fn read_tables(&self) -> Vec<(SessionId, Arc<Mutex<EdgeTable>>)> {
        self.slots
            .iter()
            .filter_map(|s| s.log.as_ref())
            .map(|l| (l.sid.clone(), Arc::clone(&l.reads)))
            .collect()
    }

    /// The slots' estimated heap together.
    pub(crate) fn charge(&self) -> usize {
        self.charge
    }

    /// Its charge recomputed from the slots (the kept sum must equal it).
    #[cfg(test)]
    pub(crate) fn recharge(&self) -> usize {
        self.slots.iter().map(ClosedSlot::charge).sum()
    }

    /// How many slots it holds, and how many of them keep a skeleton.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        (
            self.slots.len(),
            self.slots
                .iter()
                .filter(|s| s.log.as_ref().is_some_and(|l| l.skeleton.is_some()))
                .count(),
        )
    }
}
