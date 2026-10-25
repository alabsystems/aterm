// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The hosted tasks a host lists, each with a bounded timeline of its retired segments, as plain data.

use std::collections::VecDeque;

use crate::contract::Verdict;
use crate::publish::Retire;

/// The most rows a timeline keeps before it drops the oldest.
pub const TIMELINE_CAP: usize = 64;

/// What kind of task a listing hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskKind {
    /// An effect the host draws (the momentum glow).
    Effect,
}

impl TaskKind {
    /// The kind as one word.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            TaskKind::Effect => "effect",
        }
    }
}

/// What judging a retired segment found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Judged {
    /// Its replay reached the state it ended in, and every law held.
    Pass,
    /// Its replay reached the state it ended in, and `law` failed first at `cursor`.
    Fail {
        /// The law that failed.
        law: &'static str,
        /// The cursor of the first failing observation.
        cursor: u64,
    },
    /// It was not judged, for the named reason.
    Unjudged(&'static str),
}

impl From<&Verdict> for Judged {
    #[inline]
    fn from(v: &Verdict) -> Self {
        match v {
            Verdict::Pass => Judged::Pass,
            Verdict::Fail { cursor, law, .. } => Judged::Fail {
                law,
                cursor: *cursor,
            },
        }
    }
}

impl Judged {
    /// Whether this is a failing judgement (a fault to keep).
    #[inline]
    #[must_use]
    pub const fn failed(&self) -> bool {
        matches!(self, Judged::Fail { .. })
    }

    /// The judgement as one token (`pass`, `fail:<law>@<cursor>`, `unjudged:<why>`).
    #[must_use]
    pub fn token(&self) -> String {
        match self {
            Judged::Pass => "pass".to_owned(),
            Judged::Fail { law, cursor } => format!("fail:{law}@{cursor}"),
            Judged::Unjudged(why) => format!("unjudged:{}", why.replace(' ', "_")),
        }
    }
}

/// One timeline row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A retired segment and its judgement.
    Segment {
        /// Why the boundary retired it.
        cause: Retire,
        /// The name of the code that ran it.
        code: String,
        /// Its log epoch.
        log_epoch: u64,
        /// Its first cursor.
        from: u64,
        /// The cursor after its last input.
        to: u64,
        /// What judging it found.
        judged: Judged,
        /// The id of the kept fault it was pinned as, if it was.
        kept: Option<u64>,
    },
    /// The host stopped recording, for the named reason.
    Detached {
        /// Why.
        why: &'static str,
    },
}

impl Row {
    /// The row as `key=value` words after its slot and index.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Row::Segment {
                cause,
                code,
                log_epoch,
                from,
                to,
                judged,
                kept,
            } => format!(
                "segment cause={} code={code} log_epoch={log_epoch} range={from}..{to} judged={} kept={}",
                cause.word(),
                judged.token(),
                kept.map_or_else(|| "no".to_owned(), |k| k.to_string())
            ),
            Row::Detached { why } => format!("detached why={}", why.replace(' ', "_")),
        }
    }
}

/// The newest rows, at most `cap`, and how many older ones were dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timeline {
    rows: VecDeque<Row>,
    cap: usize,
    dropped: u64,
}

impl Default for Timeline {
    fn default() -> Self {
        Timeline::new(TIMELINE_CAP)
    }
}

impl Timeline {
    /// An empty timeline of at most `cap` rows.
    #[inline]
    #[must_use]
    pub const fn new(cap: usize) -> Self {
        Timeline {
            rows: VecDeque::new(),
            cap,
            dropped: 0,
        }
    }

    /// Appends `row`, dropping the oldest when the timeline is full.
    pub fn push(&mut self, row: Row) {
        if self.cap == 0 {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        if self.rows.len() >= self.cap {
            self.rows.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.rows.push_back(row);
    }

    /// The rows, oldest first.
    #[inline]
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &Row> {
        self.rows.iter()
    }

    /// How many rows it holds.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether it holds no row.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many older rows were dropped.
    #[inline]
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// One hosted task as a host lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// The host's handle for it (a window).
    pub slot: u64,
    /// What kind of task it is.
    pub kind: TaskKind,
    /// The task artifact's name.
    pub task: String,
    /// The short name of the code running it.
    pub code: String,
    /// Whether it is recorded.
    pub attached: bool,
    /// Whether its boundary is paused.
    pub paused: bool,
    /// The live pair's revision.
    pub revision: u64,
    /// The live log epoch.
    pub log_epoch: u64,
    /// The inputs in the live log.
    pub log_len: usize,
    /// Retired segments not yet judged.
    pub waiting: usize,
    /// Kept faults.
    pub kept: usize,
    /// Its timeline.
    pub timeline: Timeline,
}

impl Listing {
    /// The listing's head line.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "slot={} kind={} task={} code={} attached={} paused={} rev={} log_epoch={} log={} \
             waiting={} kept={} rows={} dropped={}",
            self.slot,
            self.kind.word(),
            self.task,
            self.code,
            self.attached,
            self.paused,
            self.revision,
            self.log_epoch,
            self.log_len,
            self.waiting,
            self.kept,
            self.timeline.len(),
            self.timeline.dropped()
        )
    }
}

/// A host's listings, in slot order, one per slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    listings: Vec<Listing>,
}

impl Registry {
    /// An empty registry.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Registry {
            listings: Vec::new(),
        }
    }

    /// Adds `listing`, replacing any listing of its slot.
    pub fn insert(&mut self, listing: Listing) {
        match self
            .listings
            .binary_search_by_key(&listing.slot, |l| l.slot)
        {
            Ok(at) => self.listings[at] = listing,
            Err(at) => self.listings.insert(at, listing),
        }
    }

    /// The listings, in slot order.
    #[inline]
    #[must_use]
    pub fn listings(&self) -> &[Listing] {
        &self.listings
    }

    /// `tasks=<n>`, then each listing's line followed by its rows (`slot=<s> row=<k> …`), oldest first.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("tasks={}", self.listings.len())];
        for l in &self.listings {
            out.push(l.line());
            for (k, row) in l.timeline.rows().enumerate() {
                out.push(format!("slot={} row={k} {}", l.slot, row.words()));
            }
        }
        out
    }
}
