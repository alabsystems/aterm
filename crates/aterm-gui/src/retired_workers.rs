// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Workers whose tabs aterm closed once their tasks concluded (`App::reap_workers`), kept so what
//! they left outlives their tabs: `aterm ctl @<sid> result` and `task` still answer for one, and
//! `await task` naming it answers at once, `closed` with its end, to its owner, an edge its session
//! granted a read and a fleet driver (`control::retired_record`); its result file stays under
//! `<state>/tasks/` until its row goes ([`forget_results`]), and the Workers page lists it under
//! Finished, with Read Result and Dismiss. A person's own close of a worker keeps no row:
//! what they closed goes, as any tab does.
//!
//! The rows live in this process's memory ([`crate::session_store::SessionStore`] holds them, so
//! the control thread reads them): at most [`RETIRED_MAX`], each for [`RETIRED_FOR_MS`] of wall
//! time, and none past a plain quit, which takes their result files with them. A seamless update
//! carries them to its successor ([`RetiredWorkers::carry_rows`], [`RetiredWorkers::adopt`]),
//! which answers for them as this process did. Dismiss, Clear Finished and a close of the Workers
//! tab take rows off the page and nothing more ([`RetiredWorkers::unlist_local`],
//! [`RetiredWorkers::unlist_window`]): a manager may not have read what its worker left yet, so
//! only age, the count or a quit drops a row.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use aterm_session::SessionId;

use crate::WindowId;
use crate::task_record::TaskRecord;

/// The most rows kept: a new one past it pushes out the oldest.
pub(crate) const RETIRED_MAX: usize = 64;

/// How long a row is kept, unix milliseconds of wall time: a day, long enough for a manager that
/// slept or a person away from the desk to read what a worker left.
pub(crate) const RETIRED_FOR_MS: u64 = 24 * 60 * 60 * 1000;

/// One worker whose tab aterm closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Retired {
    /// Its session's stable id, which `result` and `await task` name it by.
    pub(crate) sid: SessionId,
    /// Its session's local id in this process, unique for this process's life, which its Workers
    /// row is keyed by; `None` for a row a seamless update carried, whose session was the
    /// outgoing process's.
    pub(crate) local: Option<u64>,
    /// The window whose Workers page lists it; `None` once no page does (Dismissed, Cleared, its
    /// window's Workers tab closed, or carried by a seamless update), where it still answers.
    pub(crate) window: Option<WindowId>,
    /// What its row called it at its close.
    pub(crate) title: String,
    /// Its last words at its close, as its row read them.
    pub(crate) last_words: Option<String>,
    /// Its task record at its close: its manager (`from=`), its end and its result.
    pub(crate) record: TaskRecord,
    /// When its tab closed, unix milliseconds.
    pub(crate) closed_unix_ms: u64,
}

/// The rows kept, oldest first.
#[derive(Debug, Default)]
pub(crate) struct RetiredWorkers {
    rows: VecDeque<Retired>,
}

impl RetiredWorkers {
    /// Keep `row` (one already kept for its session is replaced), and return the rows it pushed
    /// past [`RETIRED_MAX`], whose result files go ([`forget_results`]).
    #[cfg(test)]
    pub(crate) fn keep(&mut self, row: Retired) -> Vec<Retired> {
        self.rows.retain(|kept| kept.sid != row.sid);
        self.rows.push_back(row);
        let over = self.rows.len().saturating_sub(RETIRED_MAX);
        self.rows.drain(..over).collect()
    }

    /// Every row kept, oldest first.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &Retired> {
        self.rows.iter()
    }

    /// The row of session `sid`.
    pub(crate) fn by_sid(&self, sid: &SessionId) -> Option<&Retired> {
        self.rows.iter().find(|row| &row.sid == sid)
    }

    /// The row of the session that had local id `local` in this process.
    pub(crate) fn by_local(&self, local: u64) -> Option<&Retired> {
        self.rows.iter().find(|row| row.local == Some(local))
    }

    /// The rows window `window`'s Workers page lists, oldest first, each with the local id its
    /// page row is keyed by.
    pub(crate) fn listed_on(&self, window: WindowId) -> impl Iterator<Item = (u64, &Retired)> {
        self.rows
            .iter()
            .filter(move |row| row.window == Some(window))
            .filter_map(|row| Some((row.local?, row)))
    }

    /// Take the row of local id `local` off its page (Dismiss), returning the window that listed
    /// it.
    pub(crate) fn unlist_local(&mut self, local: u64) -> Option<WindowId> {
        self.rows
            .iter_mut()
            .find(|row| row.local == Some(local))
            .and_then(|row| row.window.take())
    }

    /// Take window `window`'s rows off its page (Clear Finished, or its Workers tab closed),
    /// returning whether any was listed there.
    pub(crate) fn unlist_window(&mut self, window: WindowId) -> bool {
        let mut any = false;
        for row in &mut self.rows {
            if row.window == Some(window) {
                row.window = None;
                any = true;
            }
        }
        any
    }

    /// Drop the rows kept [`RETIRED_FOR_MS`] by `now_unix_ms`, returned.
    pub(crate) fn drop_aged(&mut self, now_unix_ms: u64) -> Vec<Retired> {
        let (dropped, kept): (Vec<Retired>, Vec<Retired>) = self
            .rows
            .drain(..)
            .partition(|row| now_unix_ms.saturating_sub(row.closed_unix_ms) >= RETIRED_FOR_MS);
        self.rows = kept.into();
        dropped
    }

    /// Drop every row, returned: a plain quit's.
    pub(crate) fn drop_all(&mut self) -> Vec<Retired> {
        self.rows.drain(..).collect()
    }

    /// Every row as a seamless update's manifest carries it, at `now`, oldest first
    /// ([`Self::adopt`] reads them): `sid=`, `closed=`, `title=` and `words=` (percent-encoded),
    /// then the record's own carry row ([`TaskRecord::carry_row`]), whose keys are none of these.
    /// A seamless update is a Unix one ([`crate::session_store::SessionHandoff::from_store`]).
    #[cfg(any(unix, test))]
    pub(crate) fn carry_rows(&self, now: Instant) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mut out = format!(
                    "sid={} closed={} title={}",
                    row.sid.as_str(),
                    row.closed_unix_ms,
                    aterm_codec::pct::encode(&row.title)
                );
                if let Some(words) = &row.last_words {
                    out.push_str(" words=");
                    out.push_str(&aterm_codec::pct::encode(words));
                }
                out.push(' ');
                out.push_str(&row.record.carry_row(now));
                out
            })
            .collect()
    }

    /// Keep the rows an outgoing process carried ([`Self::carry_rows`]), read at `now` with result
    /// paths held under `tasks`, each listed on no page and keyed by its sid alone: a row that does
    /// not read, names no session id, or repeats one is skipped, and none past [`RETIRED_MAX`] is
    /// kept. A row whose result is known to be gone from `tasks` is kept naming none
    /// ([`TaskRecord::settle_result`]). Returns how many were kept.
    pub(crate) fn adopt(
        &mut self,
        rows: &[String],
        tasks: Option<&Path>,
        now: Instant,
        now_unix_ms: u64,
    ) -> usize {
        let mut kept = 0;
        for line in rows.iter().take(RETIRED_MAX) {
            let field = |key: &str| {
                line.split_whitespace()
                    .filter_map(|token| token.split_once('='))
                    .find(|(k, _)| *k == key)
                    .map(|(_, value)| value)
            };
            let Some(sid) = field("sid").filter(|sid| {
                aterm_types::control_request::Sid::parse(sid).is_some()
                    && self.by_sid(&SessionId::new(*sid)).is_none()
            }) else {
                continue;
            };
            let Some(closed_unix_ms) = field("closed").and_then(|ms| ms.parse().ok()) else {
                continue;
            };
            let Some(mut record) = TaskRecord::from_carry_row(line, tasks, now, now_unix_ms) else {
                continue;
            };
            record.settle_result(tasks);
            self.rows.push_back(Retired {
                sid: SessionId::new(sid),
                local: None,
                window: None,
                title: aterm_codec::pct::decode(field("title").unwrap_or_default()),
                last_words: field("words").map(aterm_codec::pct::decode),
                record,
                closed_unix_ms,
            });
            kept += 1;
        }
        let over = self.rows.len().saturating_sub(RETIRED_MAX);
        self.rows.drain(..over);
        kept
    }
}

/// Remove the result files `rows` kept, their rows dropped from `store`: nothing answers for them
/// now. A row's results are those its session's ID names and the one its record names under an
/// earlier ID of its tab (a worker a restore brought back before aterm closed it), which stays
/// where another record in `store` names it (`task_launch::retire_result_files`).
pub(crate) fn forget_results(store: &crate::session_store::Store, rows: &[Retired]) {
    for row in rows {
        crate::task_launch::retire_result_files(store, &row.sid, row.record.result_path.as_deref());
    }
}

#[cfg(test)]
#[path = "retired_workers_tests.rs"]
mod tests;
