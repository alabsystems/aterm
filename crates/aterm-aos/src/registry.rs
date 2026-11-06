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

/// `s` as one word of a row: whitespace and controls become `_`, so a value never splits its
/// row or the line, and an empty value is `-`.
struct TokenWord<'a>(&'a str);

impl core::fmt::Display for TokenWord<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use core::fmt::Write as _;
        if self.0.is_empty() {
            return f.write_char('-');
        }
        if !self.0.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return f.write_str(self.0);
        }
        for c in self.0.chars() {
            f.write_char(if c.is_whitespace() || c.is_control() {
                '_'
            } else {
                c
            })?;
        }
        Ok(())
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
        let mut out = String::with_capacity(24);
        self.write_token(&mut out);
        out
    }

    fn write_token(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            Judged::Pass => out.push_str("pass"),
            Judged::Fail { law, cursor } => {
                let _ = write!(out, "fail:{}@{cursor}", TokenWord(law));
            }
            Judged::Unjudged(why) => {
                let _ = write!(out, "unjudged:{}", TokenWord(why));
            }
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
    /// The person's grant to record keys aterm cannot see echoed began (`on`), or ended before
    /// its attach did, for the named reason.
    Unseen {
        /// Whether the grant began.
        on: bool,
        /// Why.
        why: &'static str,
    },
}

impl Row {
    /// The row as `key=value` words after its slot and index.
    #[must_use]
    pub fn words(&self) -> String {
        let cap = match self {
            Row::Segment { code, .. } => 96 + code.len(),
            Row::Detached { why } => 13 + why.len(),
            Row::Unseen { why, .. } => 15 + why.len(),
        };
        let mut out = String::with_capacity(cap);
        self.write_words(&mut out);
        out
    }

    fn write_words(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            Row::Segment {
                cause,
                code,
                log_epoch,
                from,
                to,
                judged,
                kept,
            } => {
                let _ = write!(
                    out,
                    "segment cause={} code={} log_epoch={log_epoch} range={from}..{to} judged=",
                    cause.word(),
                    TokenWord(code),
                );
                judged.write_token(out);
                match kept {
                    Some(k) => {
                        let _ = write!(out, " kept={k}");
                    }
                    None => out.push_str(" kept=no"),
                }
            }
            Row::Detached { why } => {
                let _ = write!(out, "detached why={}", TokenWord(why));
            }
            Row::Unseen { on, why } => {
                let _ = write!(
                    out,
                    "unseen={} why={}",
                    if *on { "on" } else { "off" },
                    TokenWord(why)
                );
            }
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
        use std::fmt::Write as _;
        let mut out = String::with_capacity(128 + self.task.len() + self.code.len());
        let _ = write!(
            out,
            "slot={} kind={} task={} code={} attached={} paused={} rev={} log_epoch={} log={} \
             waiting={} kept={} rows={} dropped={}",
            self.slot,
            self.kind.word(),
            TokenWord(&self.task),
            TokenWord(&self.code),
            self.attached,
            self.paused,
            self.revision,
            self.log_epoch,
            self.log_len,
            self.waiting,
            self.kept,
            self.timeline.len(),
            self.timeline.dropped()
        );
        out
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
        let total = 1 + self
            .listings
            .iter()
            .map(|l| 1 + l.timeline.len())
            .sum::<usize>();
        let mut out = Vec::with_capacity(total);
        out.push(format!("tasks={}", self.listings.len()));
        for l in &self.listings {
            out.push(l.line());
            for (k, row) in l.timeline.rows().enumerate() {
                use std::fmt::Write;
                let mut line = String::with_capacity(120);
                let _ = write!(line, "slot={} row={k} ", l.slot);
                row.write_words(&mut line);
                out.push(line);
            }
        }
        out
    }
}
