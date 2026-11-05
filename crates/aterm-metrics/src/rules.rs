// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rules `metrics.log` is retained by (design §4.14's subject table, §4.6 "Disk").
//!
//! A [`Retention`] built as the fabric's is (`aterm_link::broker_rules`): one rule per subject
//! class of [`crate::subject`], first match wins, then the default rule.
//!
//! | Class | Filter | astream class | Bound |
//! |---|---|---|---|
//! | raw samples | [`RAW_FILTER`] | the one prefix rule | [`RAW_AGE`], [`RAW_BYTES`] |
//! | the account journal | [`JOURNAL_FILTER`] | hold, every record | [`JOURNAL_AGE`], [`JOURNAL_BYTES`] |
//! | minute summaries | [`MINUTE_FILTER`] | hold, every record | [`MINUTE_AGE`], [`MINUTE_BYTES`] |
//! | hour summaries | [`HOUR_FILTER`] | hold, every record | [`HOUR_AGE`], [`HOUR_BYTES`] |
//! | quota rows | [`QUOTA_FILTER`] | hold, `Last` row | [`QUOTA_BYTES`] |
//! | `now` rows | [`NOW_FILTER`] | hold, `Last` row | [`NOW_BYTES`] |
//! | anything else | (default) | hold, `Last` row | [`DEFAULT_BYTES`] |
//!
//! **Highest sequence wins.** The quota and `now` classes keep each subject's `Last` row
//! ([`Keep::Last`]). On such a subject astream refuses a relayed row whose sequence is at or
//! below the row the subject holds, from the same producer, answering it as a duplicate
//! (astream `store.rs` `stage_relay`, "monotone `Last`"). So a late relayed copy of an older
//! row never replaces a newer one, whichever neighbour delivers what first, and the log stays
//! in origin order per subject. A summary class keeps every record instead: a window
//! republished at a higher revision is a new record, and the reader keeps the highest revision
//! ([`crate::merge::Counters`]).
//!
//! **Why `raw` holds the cut.** astream's one prefix rule is the class whose records below the
//! cut are simply gone; every other class is carried below it within its own bounds. Raw
//! samples are the one class nobody else reads and whose loss costs nothing outside this Mac,
//! so they are the class the cut follows.
//!
//! **The default rule.** Every subject a Mac writes falls to one of the six rules, so the
//! default takes the broker's own hidden subjects (group commits and bindings, which astream
//! carries in `<log>.state` whatever the rules) and any subject a later build adds. It keeps
//! each such subject's `Last` row, within [`DEFAULT_BYTES`], so a class this build does not
//! know cannot grow the log.
//!
//! **Bounds are constants, revised from measurements.** At these caps a Mac holds at most
//! about 160 MiB: 89 MiB of rows every Mac holds and 68 MiB that stay on it (§4.14 "Cost"). A
//! `Last` class past its bytes raises the cut by at least a quarter of them (astream
//! `SLACK_DIV`), so the `now` class's 1 MiB sets how often a busy lab's presence rows ask for a
//! cut; the churn lab (Q7) measures it.

use std::io;
use std::time::Duration;

use astream_broker::{Retention, Rule};

/// Raw samples from a source that keeps no file of its own: `/m/n-<id>/local/raw/<metric>`.
pub const RAW_FILTER: &str = "/m/*/local/raw/*";
/// The account event log, read on demand: `/m/n-<id>/local/journal`.
pub const JOURNAL_FILTER: &str = "/m/*/local/journal";
/// One summary per closed minute: `/m/n-<id>/shared/1m/<metric>`.
pub const MINUTE_FILTER: &str = "/m/*/shared/1m/*";
/// One summary per closed hour: `/m/n-<id>/shared/1h/<metric>`.
pub const HOUR_FILTER: &str = "/m/*/shared/1h/*";
/// Each account's and each agy home's current row: `/m/n-<id>/shared/quota/<cli>/<key>`.
pub const QUOTA_FILTER: &str = "/m/*/shared/quota/>";
/// Presence, needs, place and the pools a Mac could not read: `/m/n-<id>/shared/now/…`.
pub const NOW_FILTER: &str = "/m/*/shared/now/>";

const HOUR_S: u64 = 60 * 60;
const DAY_S: u64 = 24 * HOUR_S;
const MIB: u64 = 1024 * 1024;

/// How long a raw sample is kept.
pub const RAW_AGE: Duration = Duration::from_secs(DAY_S);
/// The raw samples' bytes.
pub const RAW_BYTES: u64 = 64 * MIB;
/// How long a journal event is kept: the outbox's bound today.
pub const JOURNAL_AGE: Duration = Duration::from_secs(30 * DAY_S);
/// The journal's bytes: the outbox's bound today.
pub const JOURNAL_BYTES: u64 = 4 * MIB;
/// How long a minute summary is kept.
pub const MINUTE_AGE: Duration = Duration::from_secs(2 * HOUR_S);
/// The minute summaries' bytes.
pub const MINUTE_BYTES: u64 = 16 * MIB;
/// How long an hour summary is kept: the spend fold's horizon (`harness/spend.rs` `HORIZON_S`).
pub const HOUR_AGE: Duration = Duration::from_secs(8 * DAY_S);
/// The hour summaries' bytes.
pub const HOUR_BYTES: u64 = 64 * MIB;
/// The quota rows' bytes.
pub const QUOTA_BYTES: u64 = 8 * MIB;
/// The `now` rows' bytes.
pub const NOW_BYTES: u64 = MIB;
/// The default rule's bytes.
pub const DEFAULT_BYTES: u64 = 4 * MIB;

/// The rules `metrics.log` is retained by.
///
/// # Panics
/// Never: every filter here is a constant astream's grammar takes, there is one prefix rule,
/// and the default rule has no filter (the test `astream_takes_the_metrics_rules` holds it).
#[must_use]
pub fn rules() -> Retention {
    rules_with(Bytes::CAPS).unwrap_or_else(|e| unreachable!("the metrics rules are refused: {e}"))
}

/// [`rules`] with each class's bytes scaled down: the same filters, classes and ages, so a
/// test reaches the bounds the broker cuts at with a few records.
///
/// # Panics
/// Never: astream checks the filters and classes, the same constants as [`rules`]', and
/// takes any byte bound.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn rules_scaled(bytes: Bytes) -> Retention {
    rules_with(bytes).unwrap_or_else(|e| unreachable!("the scaled metrics rules are refused: {e}"))
}

/// Each class's bytes, in the order of the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bytes {
    /// Raw samples.
    pub raw: u64,
    /// The account journal.
    pub journal: u64,
    /// Minute summaries.
    pub minute: u64,
    /// Hour summaries.
    pub hour: u64,
    /// Quota rows.
    pub quota: u64,
    /// `now` rows.
    pub now: u64,
    /// Everything else.
    pub default: u64,
}

impl Bytes {
    /// The bounds [`rules`] retains by.
    pub const CAPS: Bytes = Bytes {
        raw: RAW_BYTES,
        journal: JOURNAL_BYTES,
        minute: MINUTE_BYTES,
        hour: HOUR_BYTES,
        quota: QUOTA_BYTES,
        now: NOW_BYTES,
        default: DEFAULT_BYTES,
    };

    /// What every class may hold in all.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.raw
            .saturating_add(self.journal)
            .saturating_add(self.minute)
            .saturating_add(self.hour)
            .saturating_add(self.quota)
            .saturating_add(self.now)
            .saturating_add(self.default)
    }

    /// What the classes whose rows leave the Mac may hold: the summaries, quota and `now`.
    #[must_use]
    pub const fn shared(self) -> u64 {
        self.minute
            .saturating_add(self.hour)
            .saturating_add(self.quota)
            .saturating_add(self.now)
    }
}

fn rules_with(b: Bytes) -> io::Result<Retention> {
    Retention::new(
        vec![
            Rule::prefix(RAW_FILTER)?.max_age(RAW_AGE).max_bytes(b.raw),
            Rule::hold(JOURNAL_FILTER)?
                .max_age(JOURNAL_AGE)
                .max_bytes(b.journal),
            Rule::hold(MINUTE_FILTER)?
                .max_age(MINUTE_AGE)
                .max_bytes(b.minute),
            Rule::hold(HOUR_FILTER)?.max_age(HOUR_AGE).max_bytes(b.hour),
            Rule::hold(QUOTA_FILTER)?.last_only().max_bytes(b.quota),
            Rule::hold(NOW_FILTER)?.last_only().max_bytes(b.now),
        ],
        Rule::default_hold().max_bytes(b.default),
    )
}

/// The rules in a few words, for the node service's start line.
#[must_use]
pub fn summary() -> String {
    format!(
        "raw {} h {} MiB, journal {} days {} MiB, 1m {} h {} MiB, 1h {} days {} MiB, quota {} MiB, now {} MiB, other {} MiB",
        RAW_AGE.as_secs() / HOUR_S,
        RAW_BYTES / MIB,
        JOURNAL_AGE.as_secs() / DAY_S,
        JOURNAL_BYTES / MIB,
        MINUTE_AGE.as_secs() / HOUR_S,
        MINUTE_BYTES / MIB,
        HOUR_AGE.as_secs() / DAY_S,
        HOUR_BYTES / MIB,
        QUOTA_BYTES / MIB,
        NOW_BYTES / MIB,
        DEFAULT_BYTES / MIB,
    )
}
