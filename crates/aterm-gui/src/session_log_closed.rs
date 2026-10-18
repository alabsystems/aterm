// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLOSED-SESSION INDEX (design §6, phase 2A): what `log @<sid>` and
//! `brief @<sid>` answer once the session has closed in this process.
//!
//! The index keeps, for the last [`CLOSED_INDEX_CAP`]
//! sessions closed here:
//!
//! * a SUMMARY of each, at most [`aterm_slog::CLOSED_SUMMARY_BYTES`] (1 KiB)
//!   of text ([`aterm_slog::Log::closed_summary`]: its accounting, the
//!   session's close and open, its overlaps, and its fold's lines by
//!   priority), read back on each read as a log of its own
//!   ([`aterm_slog::Log::from_closed_summary`]), so `brief` still says how it
//!   ended and `log` still counts what it held, exactly, at the bound the
//!   design sets;
//! * a SKELETON of the newest [`CLOSED_SKELETONS`]: the log evicted
//!   level-aware to [`CLOSED_SKELETON_BYTES`] encoded, by the same policy a
//!   live ring runs, so the newest structure and detail stay readable and a
//!   reader's `since=` cursor reads it as it read the live log (the same
//!   seqs, epoch and exact gaps);
//! * the session's shared authority table and launch nonce. Retirement
//!   retains its `ReadScreen` rows and removes write grants under that same
//!   mutex. Already-resolved revokers and the index address one table;
//!   `revoke`, `disconnect` and a source's close remove its rows there.
//!   Owner operations admitted before retirement may still finish. An
//!   established edge connection reads only while `decide_edge` permits
//!   `ReadScreen`; the ordinary handshake still requires a live active
//!   session. An unauthorized read answers `ERR no such session` and
//!   reveals nothing about the closed entry.
//!
//! The entry is built at the close outside every lock but the session's own
//! timeline and its edge table, one at a time
//! (`App::retire_session_registration`), and the store's write lock is held
//! only to link it in; a read or a revoke clones an `Arc` under the store's
//! read lock and locks the grants, or renders, after it. The index counts against the
//! instance-wide cap ([`crate::App::sweep_session_log_memory`]) by the
//! entries' estimated heap. It is this process's alone: a seamless update
//! does not carry it, and a restore's `prev=` from an earlier process
//! answers `ERR no such session` (the journal, design D3, is where that will
//! live).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aterm_session::{EdgeTable, LaunchNonce, Op, SessionId};
use aterm_slog::Log;

/// The most closed sessions the index answers for (the oldest goes first).
pub(crate) const CLOSED_INDEX_CAP: usize = 512;
/// How many of the newest keep a skeleton; the rest keep a summary.
pub(crate) const CLOSED_SKELETONS: usize = 32;
/// A skeleton's encoded bytes, at most.
pub(crate) const CLOSED_SKELETON_BYTES: usize = 64 * 1024;

/// An edge row's estimated heap (its token, two session ids and a nonce in a
/// hash table's slot).
const EDGE_ROW_HEAP: usize = 128;

/// One closed session, as the index keeps it.
pub(crate) struct ClosedLog {
    sid: SessionId,
    local_id: u64,
    summary: Arc<str>,
    skeleton: Option<Arc<Log>>,
    /// The read grants, shared with every reader and revoker that clones it
    /// out of the index (never locked under the store's lock).
    reads: Arc<Mutex<EdgeTable>>,
    /// How many rows `reads` held at the close, for the estimated charge.
    read_rows: usize,
    nonce: LaunchNonce,
}

/// What a read of a closed session takes out of the index under the store's
/// read lock (`Arc` clones), to decide and render after it.
#[derive(Clone)]
pub(crate) struct ClosedRead {
    sid: SessionId,
    source: ClosedSource,
    reads: Arc<Mutex<EdgeTable>>,
    nonce: LaunchNonce,
}

impl ClosedRead {
    /// The session it was.
    pub(crate) fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// What it answers from.
    pub(crate) fn source(&self) -> ClosedSource {
        self.source.clone()
    }

    /// Whether a read under `presented` (an edge's token) is granted: the
    /// live rule (`decide_edge` for `ReadScreen` against the session's own
    /// grants and launch nonce) on the remaining shared grants. Locks the
    /// grants alone.
    pub(crate) fn edge_may_read(&self, presented: &aterm_session::EdgeToken) -> bool {
        let reads = self.reads.lock().unwrap_or_else(|p| p.into_inner());
        aterm_session::decide_edge(&reads, presented, &self.sid, Op::ReadScreen, &self.nonce)
            .is_permitted()
    }
}

/// What a read of a closed session answers from, cloned under the store's
/// read lock (an `Arc`) and read after it: the skeleton while it is kept,
/// else the summary, read back into a log of its own only then.
#[derive(Clone)]
pub(crate) enum ClosedSource {
    /// The skeleton's log.
    Skeleton(Arc<Log>),
    /// The summary's text.
    Summary(Arc<str>),
}

impl ClosedSource {
    /// The log a read renders from; `None` for a summary that does not read
    /// back (never one the index wrote).
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
            .field("entries", &self.entries.len())
            .field("charge", &self.charge)
            .finish()
    }
}

#[cfg(test)]
thread_local! {
    /// How many skeletons this thread built ([`ClosedLog::kept_from`]): what a
    /// test counts of the work a close does.
    pub(crate) static SKELETONS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How much of a closed session its entry keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosedKeep {
    /// Its summary and its skeleton: a close that may be among the newest
    /// [`CLOSED_SKELETONS`] once its batch is done.
    Skeleton,
    /// Its summary alone: one a later close of the same batch will push past
    /// the newest [`CLOSED_SKELETONS`] anyway, whose skeleton (a copy of its
    /// whole ring, evicted to 64 KiB) would be built only to be dropped.
    Summary,
}

impl ClosedKeep {
    /// What the close at `index` of a batch of `of` closes keeps: the last
    /// [`CLOSED_SKELETONS`] of them their skeletons, the rest their
    /// summaries.
    pub(crate) fn in_batch(index: usize, of: usize) -> ClosedKeep {
        if of - index <= CLOSED_SKELETONS {
            ClosedKeep::Skeleton
        } else {
            ClosedKeep::Summary
        }
    }

    /// What each of a batch of view detaches keeps, in order, given which
    /// of them close their session (`closes`, the pool's
    /// `SessionPool::drops_of`): a close keeps as [`ClosedKeep::in_batch`]
    /// says for its place among the batch's CLOSES, and a detach that closes
    /// nothing (an earlier view of a session shown twice in the batch, or a
    /// session another window shows) keeps `None`. Ranking by views instead
    /// gave a batch of 33 views and 32 closes only 31 skeletons, though the
    /// index keeps 32.
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
    /// The entry for a session whose log is `log` (just closed): its
    /// summary, its skeleton as `keep` says, and its shared `reads` table,
    /// pruned to `ReadScreen` at retirement, under its launch `nonce`.
    /// `read_rows` was sampled under that table's lock before this call.
    /// Called under that session's timeline lock alone.
    /// (Named apart from every other constructor: the lock-order census
    /// resolves a call by its name.)
    pub(crate) fn kept_from(
        log: &Log,
        sid: SessionId,
        local_id: u64,
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
            local_id,
            summary,
            skeleton,
            read_rows,
            reads,
            nonce,
        }
    }

    /// The session it was.
    #[cfg(test)]
    pub(crate) fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// What a read is answered from: the skeleton while it is kept, else the
    /// summary. O(1): an `Arc` clone.
    pub(crate) fn source(&self) -> ClosedSource {
        match &self.skeleton {
            Some(log) => ClosedSource::Skeleton(Arc::clone(log)),
            None => ClosedSource::Summary(Arc::clone(&self.summary)),
        }
    }

    /// What a read takes of it (`Arc` clones, under the store's read lock).
    pub(crate) fn read(&self) -> ClosedRead {
        ClosedRead {
            sid: self.sid.clone(),
            source: self.source(),
            reads: Arc::clone(&self.reads),
            nonce: self.nonce,
        }
    }

    /// Its read grants, for a revoke or a disconnect to lock after the
    /// store's lock is released.
    pub(crate) fn reads(&self) -> Arc<Mutex<EdgeTable>> {
        Arc::clone(&self.reads)
    }

    /// Its estimated heap: what the instance-wide cap charges it.
    pub(crate) fn footprint(&self) -> usize {
        std::mem::size_of::<ClosedLog>()
            + self.summary.len()
            + self.skeleton.as_ref().map_or(0, |s| s.footprint())
            + self.read_rows * EDGE_ROW_HEAP
    }
}

/// The index: the closed sessions' entries, oldest first.
#[derive(Default)]
pub(crate) struct ClosedLogs {
    entries: VecDeque<ClosedLog>,
    /// Σ the entries' footprints, kept as they come and go.
    charge: usize,
}

impl ClosedLogs {
    /// Link `entry` in as the newest (the rows it was a source of leave the
    /// other entries' grants in the close-time sweep,
    /// `connections::sweep_session_closed_in`, as they leave live tables).
    /// Past [`CLOSED_SKELETONS`] the oldest skeleton held is dropped, past
    /// [`CLOSED_INDEX_CAP`] the oldest entry: what this returns, for the
    /// caller to free once the store's lock is released.
    pub(crate) fn link(&mut self, entry: ClosedLog) -> Vec<Arc<Log>> {
        let mut freed = Vec::new();
        self.charge = self.charge.saturating_add(entry.footprint());
        self.entries.push_back(entry);
        let n = self.entries.len();
        if n > CLOSED_SKELETONS
            && let Some(e) = self.entries.get_mut(n - 1 - CLOSED_SKELETONS)
            && let Some(skeleton) = e.skeleton.take()
        {
            self.charge = self.charge.saturating_sub(skeleton.footprint());
            freed.push(skeleton);
        }
        while self.entries.len() > CLOSED_INDEX_CAP {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.charge = self.charge.saturating_sub(old.footprint());
            freed.extend(old.skeleton);
        }
        freed
    }

    /// The newest entry for local id `local`.
    pub(crate) fn by_local(&self, local: u64) -> Option<&ClosedLog> {
        self.entries.iter().rev().find(|e| e.local_id == local)
    }

    /// The newest entry for session `sid`.
    pub(crate) fn by_sid(&self, sid: &SessionId) -> Option<&ClosedLog> {
        self.entries.iter().rev().find(|e| e.sid == *sid)
    }

    /// Every entry's read grants, by session: the tables a
    /// closing source's rows are swept from (`Arc` clones, locked after the
    /// store's lock is released).
    pub(crate) fn read_tables(&self) -> Vec<(SessionId, Arc<Mutex<EdgeTable>>)> {
        // Include initially empty tables: an owner grant admitted before
        // retirement can finish later, and must still be revocable.
        self.entries
            .iter()
            .map(|e| (e.sid.clone(), Arc::clone(&e.reads)))
            .collect()
    }

    /// The entries' estimated heap together.
    pub(crate) fn charge(&self) -> usize {
        self.charge
    }

    /// Its charge recomputed from the entries (the kept sum must equal it).
    #[cfg(test)]
    pub(crate) fn recharge(&self) -> usize {
        self.entries.iter().map(ClosedLog::footprint).sum()
    }

    /// How many entries it holds, and how many of them keep a skeleton.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        (
            self.entries.len(),
            self.entries.iter().filter(|e| e.skeleton.is_some()).count(),
        )
    }
}
