// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The digest: exceptions that need the owner, but not now, batched into one item the
//! owner can dismiss; the log keeps them for a later look.
//!
//! Its entries are [`crate::need::Need::Later`] messages, held in a store beside
//! [`crate::MessageCenter`]'s live set, not in it, so nothing that lays out the band can
//! seat one: `eligible()`, `wanted_rows()`, `rebalance`, `settle`, `reserved_progress`,
//! `mark_seen` and eviction read only the live set. A seam somebody forgets to carve out
//! fails quiet (an id-based call answers `false`), where a `Later` row in the live set
//! would seat a band row and re-grid every tab of the window.
//!
//! This module is the store: order, cap, end times and the view. `center.rs` posts,
//! dismisses and writes the log lines.

use crate::model::{Glyph, Intent, Message, MessageId, Severity, WallStamp};
use crate::need::{DIGEST_CAP, LaterAsk};
use crate::{Duration, Instant};

/// One open exception.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The id, minted from the log's one sequence.
    pub(crate) id: MessageId,
    /// The words (its need is [`crate::need::Need::Later`]).
    pub(crate) msg: Message,
    /// The ask behind it.
    pub(crate) ask: LaterAsk,
    /// The wall clock at first ingress: the order and the expiry key on it.
    pub(crate) stamp: WallStamp,
    /// The instant this process first held the entry; with [`Self::anchor_wall`], the
    /// pair the current wall clock is computed from. For an entry adopted after a
    /// restart the anchor is the adoption's wall clock, not the old stamp, which would
    /// date the end of an entry that stood for days as minutes after it began.
    pub(crate) posted_at: Instant,
    /// The wall clock (unix ms) at [`Self::posted_at`].
    pub(crate) anchor_wall: u64,
    /// Posts folded into it that changed nothing it says.
    pub(crate) repeats: u32,
    /// When a family pass first failed to name it, until one does
    /// ([`crate::need::RESOLVE_AFTER`] later it ends `Resolved`).
    pub(crate) missed_at: Option<Instant>,
    /// When its words last changed (the text-only restate spacing).
    pub(crate) restated_at: Instant,
}

impl Entry {
    /// The wall clock at `now`, from the anchor pair.
    pub(crate) fn wall_at(&self, now: Instant) -> u64 {
        let elapsed: Duration = now.saturating_duration_since(self.posted_at);
        self.anchor_wall
            .saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
    }

    /// The instant an unnamed entry ends, if a pass has missed it.
    pub(crate) fn ends_at(&self) -> Option<Instant> {
        self.missed_at.map(|t| t + crate::need::RESOLVE_AFTER)
    }
}

/// The open exceptions, and a revision that moves only when what they say
/// does — never the center's own, so a digest change repaints no window.
#[derive(Clone, Debug, Default)]
pub(crate) struct Digest {
    items: Vec<Entry>,
    revision: u64,
}

impl Digest {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn bump(&mut self) {
        self.revision += 1;
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn find_key(&self, key: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|e| e.msg.key.as_deref() == Some(key))
    }

    pub(crate) fn find_id(&self, id: MessageId) -> Option<usize> {
        self.items.iter().position(|e| e.id == id)
    }

    pub(crate) fn get(&self, i: usize) -> &Entry {
        &self.items[i]
    }

    pub(crate) fn get_mut(&mut self, i: usize) -> &mut Entry {
        &mut self.items[i]
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.items.iter()
    }

    /// The earliest instant an unnamed entry ends on its own.
    pub(crate) fn next_end(&self) -> Option<Instant> {
        self.items.iter().filter_map(Entry::ends_at).min()
    }

    pub(crate) fn push(&mut self, e: Entry) {
        self.items.push(e);
        self.revision += 1;
    }

    pub(crate) fn remove(&mut self, i: usize) -> Entry {
        self.revision += 1;
        self.items.remove(i)
    }

    /// The entry the cap files when there are more than [`DIGEST_CAP`]:
    /// the lowest severity, oldest first — never `keep` (the newcomer).
    pub(crate) fn overflow_victim(&self, keep: MessageId) -> Option<usize> {
        if self.items.len() <= DIGEST_CAP {
            return None;
        }
        self.items
            .iter()
            .enumerate()
            .filter(|(_, e)| e.id != keep)
            .min_by_key(|(_, e)| (e.msg.severity, e.stamp, e.id))
            .map(|(i, _)| i)
    }

    /// The entries in the order a person reads them: worst first, then
    /// longest waiting, then by id — stable, so a re-assert or a restatement
    /// never moves an entry under the pointer.
    pub(crate) fn ordered(&self) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.items.iter().collect();
        v.sort_by_key(|e| (std::cmp::Reverse(e.msg.severity), e.stamp, e.id));
        v
    }
}

/// One line of the digest, as every observer reads it: the Settings card, the
/// menu-bar section, the palette and `aterm ctl messages need=later`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestEntry {
    /// The entry's id (the log's).
    pub id: MessageId,
    /// The fact's identity (its fingerprint).
    pub key: String,
    /// Error, Warn or Info.
    pub severity: Severity,
    /// The mark.
    pub glyph: Glyph,
    /// The glass-form title.
    pub title: String,
    /// Why a person is needed.
    pub why: String,
    /// The one action, in words.
    pub then: String,
    /// The authored capsules (the ask's button first).
    pub actions: Vec<Intent>,
    /// Every detail line, the reason and the action first.
    pub detail: Vec<String>,
    /// When it first came in.
    pub since: WallStamp,
    /// Posts folded into it.
    pub repeats: u32,
}

/// The digest as a whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestView {
    /// Moves only when an entry is added, changed or filed.
    pub revision: u64,
    /// Worst first, then longest waiting.
    pub entries: Vec<DigestEntry>,
}

impl DigestView {
    /// Entries in the digest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` with nothing to review.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The worst severity among the entries.
    #[must_use]
    pub fn worst(&self) -> Option<Severity> {
        self.entries.iter().map(|e| e.severity).max()
    }
}

pub(crate) fn entry_view(e: &Entry) -> DigestEntry {
    DigestEntry {
        id: e.id,
        key: e.msg.key.clone().unwrap_or_default(),
        severity: e.msg.severity,
        glyph: e.msg.glyph,
        title: e.msg.title.clone(),
        why: e.ask.why_words(),
        then: e.ask.then().words().to_string(),
        actions: e.msg.actions.clone(),
        detail: e.msg.detail.clone(),
        since: e.stamp,
        repeats: e.repeats,
    }
}

pub(crate) fn view_of(d: &Digest) -> DigestView {
    DigestView {
        revision: d.revision(),
        entries: d.ordered().into_iter().map(entry_view).collect(),
    }
}

/// Which entries [`report`] lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportScope {
    /// What is waiting for a look.
    Open,
    /// What was filed (dismissed, answered, aged out, capped, or fixed).
    Filed,
    /// Both, open first.
    All,
    /// The open ones and the newest [`RECENT_FILED`] filed: what a person
    /// opening the report a week later wants, without the whole history.
    Latest,
}

/// How many filed entries [`ReportScope::Latest`] lists.
pub const RECENT_FILED: usize = 10;

/// `aterm exceptions`: a replayed log's exceptions as plain lines, readable with aterm
/// not running (the same file and decoder as `aterm ctl messages need=later`, in a
/// person's words rather than `state=` words). `offset_s` is the local zone's offset
/// from UTC and `now_unix_ms` the reader's clock; a time on an earlier day carries its
/// date (`Fri, 18 Sep, 9:02 AM`).
#[must_use]
pub fn report(
    log: &crate::log::MessageLog,
    scope: ReportScope,
    offset_s: i64,
    now_unix_ms: u64,
) -> Vec<String> {
    use crate::center::Outcome;
    use crate::log::{LogState, Retired};
    use crate::words::{clock_words, day_heading, local_day};
    let today = local_day(now_unix_ms, offset_s);
    let said = |ms: u64| {
        let day = local_day(ms, offset_s);
        if day >= today {
            clock_words(ms, offset_s)
        } else {
            format!(
                "{}, {}",
                day_heading(today, day, true),
                clock_words(ms, offset_s)
            )
        }
    };
    let pick = |open: bool| {
        let mut v = log
            .records()
            .filter(|r| r.later && r.is_live() == open)
            .collect::<Vec<_>>();
        if open {
            // Worst first, then longest waiting: the digest's own order.
            v.sort_by_key(|r| (std::cmp::Reverse(r.severity), r.stamp.unix_ms, r.id));
        } else {
            // Newest filing first: a report read later starts at what happened last.
            v.sort_by_key(|r| std::cmp::Reverse((r.retired_unix_ms, r.id)));
        }
        v
    };
    let mut out = Vec::new();
    let section =
        |name: &str, records: Vec<&crate::log::LogRecord>, shown: usize, out: &mut Vec<String>| {
            if shown < records.len() {
                out.push(format!(
                    "{name} ({}, the newest {shown} shown)",
                    records.len()
                ));
            } else {
                out.push(format!("{name} ({})", records.len()));
            }
            for r in records.into_iter().take(shown) {
                let mark = if r.severity >= Severity::Warn {
                    "warning: "
                } else {
                    ""
                };
                out.push(format!("  {}  {mark}{}", r.id, r.title));
                let when = said(r.stamp.unix_ms);
                match &r.state {
                    LogState::Posted => out.push(format!("      since  {when}")),
                    LogState::Retired(how) => {
                        let at = r
                            .retired_unix_ms
                            .map_or_else(String::new, |ms| format!(" {}", said(ms)));
                        // A person's words, not the codec's `state=` words.
                        let word = match how {
                            Retired::Resolved(Outcome::Ok) => "fixed".to_string(),
                            Retired::Resolved(Outcome::Warn) => "ended unfixed".to_string(),
                            Retired::Evicted => "filed (list full)".to_string(),
                            Retired::Superseded { .. } => "shown as a message".to_string(),
                            // The log's `unseen`; the band queue's sense of the
                            // word is not this entry's.
                            Retired::Unseen => "aged out".to_string(),
                            Retired::Answered { label } => format!("answered {label}"),
                            other => other.as_word().to_string(),
                        };
                        out.push(format!("      since  {when} \u{00b7} {word}{at}"));
                    }
                }
                if let Some(why) = r.detail.first() {
                    out.push(format!("      why    {why}"));
                }
                // The action only while the fact may still stand: open, filed or
                // ended unfixed.
                if let Some(then) = r.detail.get(1).filter(|_| {
                    r.is_live()
                        || r.filed_exception()
                        || matches!(r.retired(), Some(Retired::Resolved(Outcome::Warn)))
                }) {
                    out.push(format!("      then   {then}"));
                }
            }
        };
    if matches!(
        scope,
        ReportScope::Open | ReportScope::All | ReportScope::Latest
    ) {
        let open = pick(true);
        let n = open.len();
        section("To look at", open, n, &mut out);
    }
    if matches!(
        scope,
        ReportScope::Filed | ReportScope::All | ReportScope::Latest
    ) {
        let filed = pick(false);
        let shown = if scope == ReportScope::Latest {
            RECENT_FILED
        } else {
            filed.len()
        };
        section("Filed", filed, shown, &mut out);
    }
    out
}
