// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two merges every fleet view reads (design §4.14 "Two kinds of value, two merge
//! rules"), and the view model with ages they answer.
//!
//! **Snapshots** (quota, agy, place, needs, unread rows: [`Snapshots`]). Each subject has one
//! writer, its origin, so a reader keeps per subject the row with the highest sequence its
//! origin gave it: a late relayed copy never replaces a newer row, whatever order the
//! neighbours deliver in (on the broker, AS1's monotone `Last` already holds this for these
//! classes; [`crate::rules`]). Readings never add up, and a Mac's row that says its roster no
//! longer lists the account ([`QuotaRow::removed`]) is no reading. For each window of an
//! account, the fleet figure is the most used *current* reading across the Macs that hold one,
//! with that reading's Mac and time, by the rule the journal's reader keeps today (`aterm-db`
//! `alab::journal::seen`, `usage`): readings are grouped by their reset, a group a whole window
//! older than the newest is the window that ended, and once every reset has passed with nothing
//! read since the view says "reset at T, not read since", never a guessed 0.
//!
//! **Counters** (window records: [`Counters`]). Each (origin, metric, window) keeps its highest
//! revision only (astream's [`Rollup`]), then summaries add across origins. A window's
//! summary holds only its origin's samples, so two identities of one width never share a
//! sample, and the merged total equals the summary of every sample every Mac recorded,
//! however many times and in whatever order the records arrive. Merging is not idempotent:
//! merging every record held, rather than each identity's latest, counts a duplicate twice
//! ([`Counters::merging_every_record`], the negative control of the merge's property test).
//!
//! Every view takes `now` from its caller (Unix seconds for rows, milliseconds for windows)
//! and reads no clock, so an injected clock drives every age and every "current".

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use astream_wire::summary::{Offered as RollupOffered, Rollup, Summary, Window};

use crate::row::{AgyRow, NeedsRow, PlaceRow, QuotaRow, Row, RowError, UnreadRow, WindowRecord};
use crate::subject::{
    AccountKey, Class, Cli, Family, Metric, Node, Subject, SubjectError, TokenClass, Width,
};

/// What an offer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    /// The first row of its subject (or revision of its window) the reader holds.
    Added,
    /// It replaced an older one.
    Replaced,
    /// The reader holds this one or a newer one; nothing changed.
    Kept,
    /// Its window is older than the reader keeps; nothing changed.
    Expired,
}

impl From<RollupOffered> for Offered {
    fn from(o: RollupOffered) -> Offered {
        match o {
            RollupOffered::Added => Offered::Added,
            RollupOffered::Replaced => Offered::Replaced,
            RollupOffered::Kept => Offered::Kept,
            RollupOffered::Expired => Offered::Expired,
        }
    }
}

/// Why a record was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferError {
    /// Its subject is not a metrics subject.
    Subject(SubjectError),
    /// A raw sample or journal event: it never leaves its Mac, so no replica merges it.
    Local,
    /// Its body is not the row its subject holds.
    Row(RowError),
}

impl fmt::Display for OfferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OfferError::Subject(e) => write!(f, "subject: {e}"),
            OfferError::Local => f.write_str("a row that stays on its Mac"),
            OfferError::Row(e) => write!(f, "row: {e}"),
        }
    }
}

impl std::error::Error for OfferError {}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// The newest row of each snapshot subject, by its origin's sequence.
///
/// `seq` orders one origin's rows of one subject: the origin's producer sequence a delivery
/// carries, or, read from one broker's `Last` face, the row's offset there (under AS1 a broker
/// keeps a `Keep::Last` subject's records in origin order). One reader takes one kind of order.
#[derive(Clone, Debug, Default)]
pub struct Snapshots {
    rows: BTreeMap<Subject, (u64, Row)>,
}

impl Snapshots {
    /// An empty reader.
    #[must_use]
    pub const fn new() -> Snapshots {
        Snapshots {
            rows: BTreeMap::new(),
        }
    }

    /// Offer the row `body` at `subject`, the `seq`-th of its origin.
    ///
    /// # Errors
    /// [`OfferError::Local`] for a subject that stays on its Mac, [`OfferError::Row`] for a
    /// body that is not the row its subject holds (or a summary subject's, which
    /// [`Counters`] reads).
    pub fn offer(
        &mut self,
        subject: Subject,
        seq: u64,
        body: &[u8],
    ) -> Result<Offered, OfferError> {
        if !subject.class().travels() {
            return Err(OfferError::Local);
        }
        if !subject.class().keeps_last() {
            return Err(OfferError::Row(RowError::NoCodec));
        }
        let row = Row::decode_at(&subject, body).map_err(OfferError::Row)?;
        Ok(match self.rows.entry(subject) {
            std::collections::btree_map::Entry::Vacant(vacant) => {
                vacant.insert((seq, row));
                Offered::Added
            }
            std::collections::btree_map::Entry::Occupied(mut held) if seq > held.get().0 => {
                held.insert((seq, row));
                Offered::Replaced
            }
            std::collections::btree_map::Entry::Occupied(_) => Offered::Kept,
        })
    }

    /// The row held at `subject`, with its sequence.
    #[must_use]
    pub fn get(&self, subject: &Subject) -> Option<(u64, &Row)> {
        self.rows.get(subject).map(|(seq, row)| (*seq, row))
    }

    /// How many subjects it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn rows(&self) -> impl Iterator<Item = (&Subject, &Row)> + '_ {
        self.rows.iter().map(|(s, (_, row))| (s, row))
    }
}

/// One Mac's row, and how old its reading is at the view's `now`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacRow<'a, T> {
    /// The Mac.
    pub node: Node,
    /// Its row.
    pub row: &'a T,
    /// Seconds since the row was written (0 for a clock ahead of the reader's).
    pub age_s: u64,
}

/// The tolerance a reset gets: the clock-skew bound the journal's reader gives a reset of
/// unknown resolution (`seen.rs` `tolerance(None)`), since a row does not say how its reset was
/// read.
pub const RESET_TOLERANCE_S: u64 = 900;
/// The 5-hour window's length.
pub const FIVE_HOUR_S: u64 = 5 * 3_600;
/// The week's length.
pub const WEEK_S: u64 = 7 * 86_400;
/// A window at or past this many points is near its wall.
pub const NEAR_WALL_PCT: u16 = 90;

/// One account window's figure across the Macs that read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usage {
    /// The most used current reading.
    Current {
        /// Its points.
        pct: u16,
        /// The window's reset, when the readings agree on one.
        reset: Option<u64>,
        /// When it was read, Unix seconds.
        read: u64,
        /// The Mac that read it.
        by: Node,
    },
    /// Every window read has reset and nothing was read since.
    ResetUnread {
        /// The last reset read; past a reading that named none, the latest its window can have
        /// reset by, the reading's time plus the window's length.
        reset: u64,
        /// The newest reading's time.
        read: u64,
        /// Its Mac.
        by: Node,
    },
}

impl Usage {
    /// The current figure's points.
    #[must_use]
    pub const fn pct(self) -> Option<u16> {
        match self {
            Usage::Current { pct, .. } => Some(pct),
            Usage::ResetUnread { .. } => None,
        }
    }
}

/// One Mac's reading of one window.
#[derive(Clone, Copy, Debug)]
struct Reading {
    pct: u16,
    t: u64,
    reset: Option<u64>,
    by: Node,
}

/// The reading a figure is credited to: the most used, the newest among equals, then the
/// greater node id (a total order, so every Mac credits the same one).
fn figure<'r>(list: impl IntoIterator<Item = &'r Reading>) -> Option<&'r Reading> {
    list.into_iter()
        .max_by(|a, b| (a.pct, a.t, a.by).cmp(&(b.pct, b.t, b.by)))
}

/// The newest reading, the greater node id among equals.
fn newest<'r>(list: impl IntoIterator<Item = &'r Reading>) -> Option<&'r Reading> {
    list.into_iter().max_by_key(|r| (r.t, r.by))
}

/// One window's figure from its readings at `now`: the journal reader's rule
/// (`seen.rs` `usage`), every reset at [`RESET_TOLERANCE_S`], with one bound more. A reading
/// that names no reset still speaks only for a window `length` long, so once a whole window
/// (and the tolerance) has passed since it was read, its window has reset, and it is no
/// current figure.
fn usage(length: u64, readings: &[Reading], now: u64) -> Option<Usage> {
    let current_of = |r: &Reading| Usage::Current {
        pct: r.pct,
        reset: None,
        read: r.t,
        by: r.by,
    };
    let reset_by = |r: &Reading| r.t.saturating_add(length);
    let fresh = |r: &&Reading| reset_by(r).saturating_add(RESET_TOLERANCE_S) > now;
    let reset_of = |r: &Reading| r.reset.unwrap_or(0);
    let Some(last_reset) = readings.iter().filter_map(|r| r.reset).max() else {
        // No reading names its window: the newest one, its reset unknown, while its window
        // can still be the current one.
        let last = newest(readings.iter())?;
        return Some(if fresh(&last) {
            current_of(last)
        } else {
            Usage::ResetUnread {
                reset: reset_by(last),
                read: last.t,
                by: last.by,
            }
        });
    };
    let mut current: Vec<&Reading> = readings
        .iter()
        .filter(|r| {
            r.reset
                .is_some_and(|res| res.saturating_add(RESET_TOLERANCE_S) > now)
        })
        .collect();
    if current.is_empty() {
        // Every reset has passed: only a reading made since speaks for the new window, while
        // that window can still be the current one.
        let since = || {
            readings
                .iter()
                .filter(|r| r.reset.is_none() && r.t > last_reset)
        };
        if let Some(fig) = figure(since().filter(fresh)) {
            return Some(current_of(fig));
        }
        let last =
            newest(since()).or_else(|| newest(readings.iter().filter(|r| r.reset.is_some())))?;
        let reset = since().map(reset_by).fold(last_reset, u64::max);
        return Some(Usage::ResetUnread {
            reset,
            read: last.t,
            by: last.by,
        });
    }
    // Group by window identity: sorted by reset, a reading joins the group before it when the
    // two resets lie within the tolerance.
    current.sort_unstable_by_key(|r| (r.reset, r.t, r.by));
    let mut groups: Vec<&[&Reading]> = Vec::new();
    let mut group_start = 0;
    for i in 1..current.len() {
        if reset_of(current[i]).abs_diff(reset_of(current[i - 1])) > RESET_TOLERANCE_S {
            groups.push(&current[group_start..i]);
            group_start = i;
        }
    }
    groups.push(&current[group_start..]);
    // The newest group names the current window; a group a whole window before it is the
    // window that ended, not a disagreement about this one.
    let newest_reset = groups.last()?.iter().map(|r| reset_of(r)).min()?;
    let overlapping: Vec<&&[&Reading]> = groups
        .iter()
        .filter(|g| {
            let reset = g.iter().map(|r| reset_of(r)).max().unwrap_or(0);
            newest_reset
                .saturating_sub(reset)
                .saturating_add(RESET_TOLERANCE_S)
                < length
        })
        .collect();
    let start = overlapping
        .iter()
        .flat_map(|g| g.iter())
        .map(|r| reset_of(r))
        .min()
        .map_or(0, |reset| reset.saturating_sub(length));
    let fig = figure(
        overlapping.iter().flat_map(|g| g.iter().copied()).chain(
            readings
                .iter()
                .filter(|r| r.reset.is_none() && r.t >= start),
        ),
    )?;
    let reset = match overlapping.as_slice() {
        // Every reset has one tolerance, so the newest reading's reset.
        [one] => one.iter().max_by_key(|r| (r.t, r.by)).and_then(|r| r.reset),
        _ => None,
    };
    Some(Usage::Current {
        pct: fig.pct,
        reset,
        read: fig.t,
        by: fig.by,
    })
}

/// An account as the fleet holds it at `now`: each Mac's row with its age, and the merged
/// figures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView<'a> {
    /// The CLI.
    pub cli: Cli,
    /// The account.
    pub account: AccountKey,
    /// Each Mac's row, by node id.
    pub macs: Vec<MacRow<'a, QuotaRow>>,
    /// The 5-hour window's figure.
    pub five_hour: Option<Usage>,
    /// The week's figure.
    pub week: Option<Usage>,
    /// A wall that stands at `now`: its latest end, and the Mac that reads it.
    pub wall: Option<(Node, u64)>,
    /// The latest use on any Mac, and that Mac.
    pub last_used: Option<(Node, u64)>,
}

impl AccountView<'_> {
    /// Whether it is near a wall: a wall stands, or a current window is at
    /// [`NEAR_WALL_PCT`] or more.
    #[must_use]
    pub fn near_wall(&self) -> bool {
        self.wall.is_some()
            || [self.five_hour, self.week]
                .into_iter()
                .flatten()
                .filter_map(Usage::pct)
                .any(|p| p >= NEAR_WALL_PCT)
    }
}

fn age(at: u64, now: u64) -> u64 {
    now.saturating_sub(at)
}

impl Snapshots {
    /// Every account any Mac holds a row of, merged at `now` (Unix seconds), by CLI then key. A
    /// Mac's row that says its roster no longer lists the account ([`QuotaRow::removed`]) is
    /// left out, so an account every Mac removed is not listed.
    #[must_use]
    pub fn accounts(&self, now: u64) -> Vec<AccountView<'_>> {
        let mut by_account: BTreeMap<(Cli, AccountKey), Vec<MacRow<'_, QuotaRow>>> =
            BTreeMap::new();
        for (subject, row) in self.rows() {
            if let (Subject::Quota { node, cli, account }, Row::Quota(q)) = (subject, row)
                && !q.removed()
            {
                by_account
                    .entry((*cli, *account))
                    .or_default()
                    .push(MacRow {
                        node: *node,
                        row: q,
                        age_s: age(q.at, now),
                    });
            }
        }
        by_account
            .into_iter()
            .map(|((cli, account), macs)| {
                let readings =
                    |pct: fn(&QuotaRow) -> Option<u16>, reset: fn(&QuotaRow) -> Option<u64>| {
                        macs.iter()
                            .filter_map(|m| {
                                pct(m.row).map(|p| Reading {
                                    pct: p,
                                    t: m.row.at,
                                    reset: reset(m.row),
                                    by: m.node,
                                })
                            })
                            .collect::<Vec<_>>()
                    };
                let five = readings(|r| r.pct5, |r| r.five_hour_reset);
                let week = readings(|r| r.pct7, |r| r.week_reset);
                let wall = macs
                    .iter()
                    .filter_map(|m| m.row.walled_until.filter(|&u| u > now).map(|u| (m.node, u)))
                    .max_by_key(|&(node, u)| (u, node));
                let last_used = macs
                    .iter()
                    .filter_map(|m| m.row.last_used.map(|t| (m.node, t)))
                    .max_by_key(|&(node, t)| (t, node));
                AccountView {
                    cli,
                    account,
                    five_hour: usage(FIVE_HOUR_S, &five, now),
                    week: usage(WEEK_S, &week, now),
                    wall,
                    last_used,
                    macs,
                }
            })
            .collect()
    }

    /// Each Mac's agy homes, with their ages.
    #[must_use]
    pub fn agy(&self, now: u64) -> Vec<MacRow<'_, AgyRow>> {
        self.rows()
            .filter_map(|(s, row)| match row {
                Row::Agy(r) => Some(MacRow {
                    node: s.node(),
                    row: r,
                    age_s: age(r.at, now),
                }),
                _ => None,
            })
            .collect()
    }

    /// The placements that hold at `now`.
    #[must_use]
    pub fn places(&self, now: u64) -> Vec<MacRow<'_, PlaceRow>> {
        self.rows()
            .filter_map(|(s, row)| match row {
                Row::Place(r) if r.live(now) => Some(MacRow {
                    node: s.node(),
                    row: r,
                    age_s: age(r.at, now),
                }),
                _ => None,
            })
            .collect()
    }

    /// Each Mac's needs row, with its age.
    #[must_use]
    pub fn needs(&self, now: u64) -> Vec<MacRow<'_, NeedsRow>> {
        self.rows()
            .filter_map(|(s, row)| match row {
                Row::Needs(r) => Some(MacRow {
                    node: s.node(),
                    row: r,
                    age_s: age(r.at, now),
                }),
                _ => None,
            })
            .collect()
    }

    /// Each Mac's unread pools, with their age.
    #[must_use]
    pub fn unread(&self, now: u64) -> Vec<MacRow<'_, UnreadRow>> {
        self.rows()
            .filter_map(|(s, row)| match row {
                Row::Unread(r) => Some(MacRow {
                    node: s.node(),
                    row: r,
                    age_s: age(r.at, now),
                }),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

/// Every window record offered: per (origin, metric, window) the highest revision, added
/// across origins on read.
#[derive(Clone, Debug, Default)]
pub struct Counters {
    rollup: Rollup,
    /// Each Mac's latest window start per width, Unix milliseconds: what a view shows of a Mac
    /// that went quiet.
    latest: BTreeMap<(Node, Width), u64>,
    /// The negative control's held records: every one offered, merged as they are.
    #[cfg(any(test, debug_assertions))]
    every: Option<Vec<WindowRecord>>,
}

impl Counters {
    /// An empty reader.
    #[must_use]
    pub const fn new() -> Counters {
        Counters {
            rollup: Rollup::new(),
            latest: BTreeMap::new(),
            #[cfg(any(test, debug_assertions))]
            every: None,
        }
    }

    /// THE NEGATIVE CONTROL: a reader that merges every record it was offered, not each
    /// identity's highest revision. A relayed duplicate or a republished window then counts
    /// twice, which the merge's property test shows; never built in a release.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn merging_every_record() -> Counters {
        Counters {
            rollup: Rollup::new(),
            latest: BTreeMap::new(),
            every: Some(Vec::new()),
        }
    }

    /// Offer one window record.
    pub fn offer(&mut self, record: WindowRecord) -> Offered {
        let width = record.metric.width();
        let start = record.window.start_ms();
        #[cfg(any(test, debug_assertions))]
        if let Some(every) = &mut self.every {
            every.push(record.clone());
        }
        let offered = Offered::from(self.rollup.offer(
            record.id(),
            record.revision,
            record.summary,
        ));
        if offered != Offered::Expired {
            let latest = self.latest.entry((record.node, width)).or_insert(start);
            *latest = (*latest).max(start);
        }
        offered
    }

    /// The merge over every Mac of `metric`'s windows that start in `span` (Unix
    /// milliseconds). For a span whose ends are window boundaries of the metric's width it
    /// equals the summary of every sample every Mac recorded in it.
    #[must_use]
    pub fn summarize(&self, metric: Metric, span: Range<u64>) -> Summary {
        #[cfg(any(test, debug_assertions))]
        if let Some(every) = &self.every {
            return every
                .iter()
                .filter(|r| r.metric == metric && span.contains(&r.window.start_ms()))
                .fold(Summary::new(), |acc, r| acc.merged(&r.summary));
        }
        self.rollup
            .summarize(&metric.metric_name(), metric.width().window_len(), span)
    }

    /// [`Counters::summarize`], one summary per Mac.
    #[must_use]
    pub fn per_node(&self, metric: Metric, span: Range<u64>) -> BTreeMap<Node, Summary> {
        #[cfg(any(test, debug_assertions))]
        if let Some(every) = &self.every {
            let mut out: BTreeMap<Node, Summary> = BTreeMap::new();
            for r in every
                .iter()
                .filter(|r| r.metric == metric && span.contains(&r.window.start_ms()))
            {
                out.entry(r.node).or_default().merge(&r.summary);
            }
            return out;
        }
        self.rollup
            .per_origin(&metric.metric_name(), metric.width().window_len(), span)
            .into_iter()
            .filter_map(|(origin, s)| Node::parse(origin.as_str()).map(|n| (n, s)))
            .collect()
    }

    /// The latest window of `width` `node` has a record of: its start, Unix milliseconds.
    #[must_use]
    pub fn latest_window(&self, node: Node, width: Width) -> Option<u64> {
        self.latest.get(&(node, width)).copied()
    }

    /// Forget every record of `width` whose window ended at or before `at_ms`, and refuse such
    /// records from now on; returns how many went. A reader bounds its memory by the class's
    /// retention: minute rows for 2 hours, hour rows for 8 days.
    pub fn forget_before(&mut self, width: Width, at_ms: u64) -> usize {
        self.rollup.forget_before(width.window_len(), at_ms)
    }

    /// How many identities it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rollup.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rollup.is_empty()
    }
}

/// One Mac's (or the lab's) tokens in one hour.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Tokens per CLI.
    pub by_cli: BTreeMap<Cli, u128>,
    /// Messages or turns per CLI.
    pub turns: BTreeMap<Cli, u64>,
    /// Tokens per CLI and model family, from the hour records; empty for an hour only its
    /// minutes hold.
    pub by_family: BTreeMap<(Cli, Family), u128>,
    /// Whether some CLI's tokens come from minute records, which carry no family (the current
    /// hour, or an hour whose hour records have not all arrived): its families then add up to
    /// less than its tokens.
    pub from_minutes: bool,
}

impl Tokens {
    /// Every CLI's tokens together.
    #[must_use]
    pub fn total(&self) -> u128 {
        self.by_cli
            .values()
            .copied()
            .fold(0u128, u128::saturating_add)
    }

    fn add(&mut self, other: &Tokens) {
        for (cli, n) in &other.by_cli {
            let slot = self.by_cli.entry(*cli).or_default();
            *slot = slot.saturating_add(*n);
        }
        for (cli, n) in &other.turns {
            let slot = self.turns.entry(*cli).or_default();
            *slot = slot.saturating_add(*n);
        }
        for (key, n) in &other.by_family {
            let slot = self.by_family.entry(*key).or_default();
            *slot = slot.saturating_add(*n);
        }
        self.from_minutes |= other.from_minutes;
    }
}

/// The lab's tokens in one hour, per Mac and in all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HourTokens {
    /// The hour's start, Unix milliseconds.
    pub start_ms: u64,
    /// Whether the hour had ended at the view's `now`.
    pub closed: bool,
    /// Each Mac that recorded any.
    pub by_node: BTreeMap<Node, Tokens>,
    /// Every Mac together.
    pub total: Tokens,
}

impl Counters {
    /// Tokens per hour for each hour that starts in `span` (Unix milliseconds), per Mac and in
    /// all, at `now_ms`.
    ///
    /// A Mac's hour is held twice: by its hour records (one per family and token class) and
    /// by its minute totals, the same samples. Never both are added. Each copy may be partly
    /// delivered (the hour's records arrive one at a time, and minute records age out after
    /// two hours), and each is a lower bound on the hour, so for each Mac and CLI the larger
    /// of the two counts, tokens and turns apart. The families come from the hour records.
    #[must_use]
    pub fn tokens_per_hour(&self, span: Range<u64>, now_ms: u64) -> Vec<HourTokens> {
        let mut out = Vec::new();
        let Some(mut hour) = Window::containing(span.start, Width::Hour.window_len()) else {
            return out;
        };
        if hour.start_ms() < span.start {
            let Some(next) = hour.next() else {
                return out;
            };
            hour = next;
        }
        while hour.start_ms() < span.end {
            out.push(self.hour_tokens(hour, now_ms));
            let Some(next) = hour.next() else {
                break;
            };
            hour = next;
        }
        out
    }

    fn hour_tokens(&self, hour: Window, now_ms: u64) -> HourTokens {
        let span = hour.span();
        let mut by_node: BTreeMap<Node, Tokens> = BTreeMap::new();
        for metric in Metric::ALL {
            if let Metric::Class(cli, family, class) = metric {
                for (node, s) in self.per_node(metric, span.clone()) {
                    let t = by_node.entry(node).or_default();
                    let fam = t.by_family.entry((cli, family)).or_default();
                    *fam = fam.saturating_add(s.sum());
                    let c = t.by_cli.entry(cli).or_default();
                    *c = c.saturating_add(s.sum());
                    if class == TokenClass::Output {
                        let n = t.turns.entry(cli).or_default();
                        *n = n.saturating_add(s.count());
                    }
                }
            }
        }
        for metric in Metric::ALL {
            if let Metric::Tokens(cli) = metric {
                for (node, s) in self.per_node(metric, span.clone()) {
                    let t = by_node.entry(node).or_default();
                    let c = t.by_cli.entry(cli).or_default();
                    if s.sum() > *c {
                        *c = s.sum();
                        t.from_minutes = true;
                    }
                    let n = t.turns.entry(cli).or_default();
                    *n = (*n).max(s.count());
                }
            }
        }
        let mut total = Tokens::default();
        for t in by_node.values() {
            total.add(t);
        }
        HourTokens {
            start_ms: hour.start_ms(),
            closed: hour.end_ms() <= now_ms,
            by_node,
            total,
        }
    }
}

// ---------------------------------------------------------------------------
// The replica: both merges
// ---------------------------------------------------------------------------

/// A Mac's copy of every Mac's shared rows: what each fleet view reads.
#[derive(Clone, Debug, Default)]
pub struct Replica {
    /// Quota, agy, place, needs and unread rows.
    pub snapshots: Snapshots,
    /// Window records.
    pub counters: Counters,
}

impl Replica {
    /// An empty copy.
    #[must_use]
    pub const fn new() -> Replica {
        Replica {
            snapshots: Snapshots::new(),
            counters: Counters::new(),
        }
    }

    /// Offer one record as a broker delivers it: its subject, its origin's sequence (or its
    /// offset on a `Last` face, [`Snapshots`]) and its row.
    ///
    /// `body` is the row itself. A shared record's body on the log is astream's origin-signed
    /// envelope (`astream_broker::origin`, under its `relay` feature: a 64-byte signature,
    /// then the row), which the broker checked before appending it; the reader that adopts
    /// `relay` (B7a's node service, Q5's views) hands this the envelope's payload.
    ///
    /// # Errors
    /// [`OfferError`]: not a metrics subject, a subject that stays on its Mac, or a body that
    /// is not the row its subject holds.
    pub fn offer(&mut self, subject: &str, seq: u64, body: &[u8]) -> Result<Offered, OfferError> {
        let subject = Subject::parse(subject).map_err(OfferError::Subject)?;
        match subject.class() {
            Class::Raw | Class::Journal => Err(OfferError::Local),
            Class::Minute | Class::Hour => match Row::decode_at(&subject, body) {
                Ok(Row::Window(record)) => Ok(self.counters.offer(record)),
                Ok(_) => Err(OfferError::Row(RowError::NoCodec)),
                Err(e) => Err(OfferError::Row(e)),
            },
            Class::Quota | Class::Now => self.snapshots.offer(subject, seq, body),
        }
    }
}
