// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The readings series: each account label's history of usage figures on one machine, the
//! history file `docs/DESIGN-account-stats-2026-10-15.md` §5 designs. The accounts outlook's
//! pace reads its newest week (`docs/DESIGN-accounts-outlook-2026-10-25.md` §4).
//!
//! The series lives beside the accounts store, never in it: `<harness_state>/accounts/stats/`
//! holds one `u-<label>@<machine>.jsonl` per label and machine, a directory of mode 0700 and
//! files of mode 0600, under its own lock (`stats/.lock`), never `accounts.lock`. A row carries a
//! label's figures, the record's keys (`ac`, `key`) and the machine that read them, and never an
//! email, a uuid or prose.
//!
//! Rows, each `"v":1` and at most [`ROW_MAX`] bytes:
//! - `fig`: a recorded tap reading's windows; `beat`: a recorded reading whose figures and keys
//!   equal the last `fig` row's; `plan`: a row's tier, from the footer's gated profile read;
//!   `event`: a closed word a store transaction committed, written after it: `another-account`
//!   (the tap's), `wall` (a model's week a session's screen said is at its wall) and
//!   `grant-seen` (a limit-reset offer first seen or changed).
//! - `close` and `day`, derived from the `fig` rows by [`compact_locked`]: one per window anchor
//!   once it ended ([`derive_closes`]), one per local day once it ended ([`day_summaries`]).
//!
//! A compaction keeps every row for [`RAW_S`], the last `fig` of each UTC hour (and where each
//! weekly run begins) and every other row but beats to [`HOURLY_S`], the `day` rows, weekly
//! closes, events, plans and rows of unknown kinds to [`DAILY_S`], and past that only the newest
//! `plan` row; a file past [`BOUND_BYTES`] gives up rows in a fixed order. It writes every kept
//! row back as it read it.
//!
//! Its readers write nothing: `accounts stats` and the accounts report say how far each window
//! moved over a span ([`since_of`]), and `accounts history` an account's days ([`day_summaries`]
//! over the rows with the closes derived in memory, [`resets_used`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::{Map, Value};
use aterm_types::MutexExt as _;

use super::accounts::{self, WindowFig};
use super::profile::AccountKey;

/// The directory under the accounts store the series live in.
pub const DIR: &str = "stats";

/// The series' own lock file in [`DIR`].
pub const LOCK_FILE: &str = ".lock";

/// The stamp in [`DIR`] of the last pass over every series ([`compact_dir`]): decimal Unix
/// seconds, mode 0600.
pub const COMPACTED_FILE: &str = ".compacted";

/// The row schema. It is add-only: a row of a kind or with fields a reader does not know is
/// kept as it was written.
pub const SCHEMA: u64 = 1;

/// Every row is kept this long.
pub const RAW_S: i64 = 7 * 86_400;

/// How far back from now the outlook reads `fig` and `beat` rows ([`outlook_view`]): one week and
/// the week's pace's lookback ([`super::outlook::WEEK_LOOKBACK_S`]), so a row before the lookback
/// says the series covers it.
pub const VIEW_S: i64 = RAW_S + super::outlook::WEEK_LOOKBACK_S;

/// Every row is kept this long in a file past [`BOUND_BYTES`].
pub const RAW_SHRUNK_S: i64 = 3 * 86_400;

/// The last `fig` row of each UTC hour, and every row but beats, are kept this long.
pub const HOURLY_S: i64 = 90 * 86_400;

/// `day` rows, weekly closes, events, `plan` rows and rows of unknown kinds are kept this long;
/// a series with no row inside it is removed.
pub const DAILY_S: i64 = 400 * 86_400;

/// The most bytes a compacted series file holds.
pub const BOUND_BYTES: u64 = 2 * 1024 * 1024;

/// How far under [`BOUND_BYTES`] a compaction the bound made leaves the file, so the appends
/// after it have room before the next pass.
pub const BOUND_ROOM_BYTES: u64 = BOUND_BYTES / 8;

/// The most bytes a reader takes of one file, and a compaction of its end.
pub const READ_MAX: u64 = 2 * BOUND_BYTES;

/// How often an append runs a pass over every series ([`compact_due`]).
pub const COMPACT_EVERY_S: i64 = 86_400;

/// A window anchor is closed this long after its reset, where no newer anchor came first.
pub const CLOSE_SLACK_S: i64 = 600;

/// The most limit resets used one `close` row names.
pub const EPOCHS_MAX: usize = 4;

/// The most bytes one row may take.
pub const ROW_MAX: usize = 512;

/// How long a writer waits for [`LOCK_FILE`] before it drops its row (an `event` or `plan` row
/// waits in this process's outbox for the next append to the file).
pub const LOCK_WAIT: Duration = Duration::from_millis(50);

/// The most rows the outbox holds for one file.
const OUTBOX_MAX: usize = 16;

/// What a derived `close` row says it came from.
pub const SRC_SERIES: &str = "series";

/// How far back from a file's end a writer looks for the last `fig` row.
const LAST_FIG_BYTES: u64 = 4096;

const DAY_S: i64 = 86_400;

/// One window of a `fig` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fig {
    /// Tenths of a percent used.
    pub pct_x10: u32,
    /// The window's reset.
    pub reset: i64,
    /// When the figure was served.
    pub at: i64,
    /// Where it came from ([`accounts::SOURCE_TAP`], [`accounts::SOURCE_SAMPLE`]).
    pub src: String,
    /// A limit reset used inside the window ([`WindowFig::epoch_at`]).
    pub epoch: Option<i64>,
}

impl Fig {
    /// The series' copy of a store figure.
    #[must_use]
    pub fn of(fig: &WindowFig) -> Fig {
        Fig {
            pct_x10: fig.pct_x10,
            reset: fig.reset,
            at: fig.at,
            src: fig.source.clone(),
            epoch: fig.epoch_at,
        }
    }

    fn same_figure(&self, other: &Fig) -> bool {
        (self.pct_x10, self.reset, self.epoch) == (other.pct_x10, other.reset, other.epoch)
    }

    /// The figure in whole points, rounded half up ([`WindowFig::whole`]).
    #[must_use]
    pub const fn whole(&self) -> u32 {
        self.pct_x10.saturating_add(5) / 10
    }
}

/// A row's tier, as the footer's gated profile read named it ([`note_plan`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlanSaid {
    /// The plan's word, lower case (`max`, `pro`).
    pub word: String,
    /// Its tier where one was named (`20x`).
    pub tier: Option<String>,
    /// What named it (`profile`).
    pub src: String,
}

impl PlanSaid {
    /// Whether `other` names the same plan: its word and its tier.
    #[must_use]
    pub fn says(&self, other: &PlanSaid) -> bool {
        self.word == other.word && self.tier == other.tier
    }
}

/// One window anchor that ended: what its `fig` rows read of it ([`derive_closes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    /// The window ([`accounts::SESSION_LIMIT`], [`accounts::WEEK_LIMIT`]).
    pub w: &'static str,
    /// The anchor: the window's reset.
    pub reset: i64,
    /// Its beginning, `reset − length`.
    pub from: i64,
    /// The highest figure read under it, in tenths.
    pub peak: u32,
    /// The last figure read under it, in tenths.
    pub end: u32,
    /// When its first figure was served.
    pub first: i64,
    /// When its last figure was served.
    pub last: i64,
    /// How many figures were read under it.
    pub n: u32,
    /// Its peak read full (`peak` ≥ 1000).
    pub walled: bool,
    /// The limit resets used inside it, at most [`EPOCHS_MAX`], oldest first: each figure's
    /// `epoch` and each refill drop of at least [`accounts::REFILL_MIN_X10`].
    pub epochs: Vec<i64>,
    /// A newer anchor's first figure came before this reset.
    pub early: bool,
    /// What it came from ([`SRC_SERIES`]).
    pub src: String,
    /// The record's lab key, from its newest `fig` row that held one.
    pub ac: Option<String>,
    /// The record's account key, likewise.
    pub key: Option<AccountKey>,
}

/// One local day's five-hour windows ([`Day::five_hour`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayFive {
    /// Windows closed that day.
    pub closed: u32,
    /// The highest five-hour figure read that day, in tenths (with none read, the highest peak
    /// of the windows closed that day).
    pub peak: u32,
    /// Windows closed that day that read full.
    pub walled: u32,
}

/// One local day's week ([`Day::seven_day`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayWeek {
    /// The day's first weekly figure, in tenths.
    pub first: u32,
    /// Its last.
    pub last: u32,
    /// The reset its last figure named.
    pub reset: i64,
    /// A week closed that day.
    pub closed: bool,
}

/// One local day of a series ([`day_summaries`]); the row's `t` is the day's local midnight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    /// The day's length: 23 or 25 hours across a daylight-saving change.
    pub span_s: i64,
    /// The zone's offset from UTC at its midnight, seconds east.
    pub tz_s: i64,
    /// Its five-hour windows, where any was read or closed.
    pub five_hour: Option<DayFive>,
    /// Its week, where any was read or closed.
    pub seven_day: Option<DayWeek>,
    /// The readings recorded that day (`fig` and `beat` rows).
    pub n: u32,
    /// The record's lab key, from the day's newest row that held one.
    pub ac: Option<String>,
    /// The record's account key, likewise.
    pub key: Option<AccountKey>,
}

/// An event row's word: closed, bounded, and kept as a word where a newer build wrote one this
/// build does not know.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventWord {
    /// Another account signed in to the home ([`accounts::Kind::Epoch`]).
    AnotherAccount,
    /// A model's week a session's screen said is at its wall, on record for the stats
    /// ([`accounts::WeekWalled`]): its bucket in `w`, the wall's reset where it named one.
    Wall,
    /// A limit-reset offer ([`accounts::Offer`]) first seen, or seen with another kind, count or
    /// use-by instant: its kind, its count in `n`, its use-by instant where one was read.
    GrantSeen,
    /// A word this build does not know.
    Other(String),
}

impl EventWord {
    /// The word as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            EventWord::AnotherAccount => "another-account",
            EventWord::Wall => "wall",
            EventWord::GrantSeen => "grant-seen",
            EventWord::Other(word) => word,
        }
    }

    /// `text` as an event word ([`is_word`]).
    #[must_use]
    pub fn parse(text: &str) -> Option<EventWord> {
        if !is_word(text) {
            return None;
        }
        Some(match text {
            "another-account" => EventWord::AnotherAccount,
            "wall" => EventWord::Wall,
            "grant-seen" => EventWord::GrantSeen,
            other => EventWord::Other(other.to_owned()),
        })
    }
}

/// Whether `text` is a row's word (its kind, an event): lower-case ASCII letters, digits, `-`
/// and `_`, at most [`accounts::WORD_MAX`] bytes.
#[must_use]
pub fn is_word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= accounts::WORD_MAX
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// What an `event` row says: a closed word and numbers, never a label beyond the series' own,
/// text or a vendor's flag name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSaid {
    /// What happened.
    pub e: EventWord,
    /// The window or bucket it concerns, where one does (a `wall` row's bucket id).
    pub w: Option<String>,
    /// When it happened, where that is not the row's `t`.
    pub at: Option<i64>,
    /// The reset it names, where one was named (a `wall` row's).
    pub reset: Option<i64>,
    /// The reset kind it counts, where one was named (a `grant-seen` row's `weekly`).
    pub kind: Option<String>,
    /// How many it counts (a `grant-seen` row's resets left).
    pub n: Option<u32>,
    /// Its deadline, where one was read (a `grant-seen` row's use-by instant).
    pub use_by: Option<i64>,
}

impl EventSaid {
    /// An event of `e` that says nothing more.
    #[must_use]
    pub const fn of(e: EventWord) -> EventSaid {
        EventSaid {
            e,
            w: None,
            at: None,
            reset: None,
            kind: None,
            n: None,
            use_by: None,
        }
    }

    /// The `wall` row of a model's week a session's screen said is at its wall: its bucket, the
    /// reset the wall named and when the screen said it.
    #[must_use]
    pub fn wall(walled: &accounts::WeekWalled) -> EventSaid {
        EventSaid {
            w: Some(walled.bucket.to_owned()),
            at: Some(walled.at),
            reset: walled.reset,
            ..EventSaid::of(EventWord::Wall)
        }
    }

    /// The `grant-seen` row of a limit-reset offer: its kind's word where one was named, the
    /// resets left, the use-by instant where one was read and when it was seen.
    #[must_use]
    pub fn grant_seen(offer: &accounts::Offer) -> EventSaid {
        EventSaid {
            at: Some(offer.at),
            kind: offer.kind.map(|kind| kind.word().to_owned()),
            n: Some(offer.left),
            use_by: offer.use_by,
            ..EventSaid::of(EventWord::GrantSeen)
        }
    }
}

/// What one row says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A recorded reading's windows, and the record's keys.
    Fig {
        /// The five-hour window.
        five_hour: Option<Fig>,
        /// The week.
        seven_day: Option<Fig>,
        /// The record's lab key ([`accounts::Account::pk`]).
        ac: Option<String>,
        /// The record's account key ([`accounts::Account::key`]).
        key: Option<AccountKey>,
    },
    /// A recorded reading whose figures and keys equal the last `fig` row's.
    Beat,
    /// The row's tier.
    Plan(PlanSaid),
    /// A window anchor that ended.
    Close(Close),
    /// A local day.
    Day(Day),
    /// Something the tap's transaction committed.
    Event(EventSaid),
}

/// The part of a row every schema-1 row has, read where the rest is not ([`compact_locked`]
/// keeps such a row as written).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// When it was written.
    pub t: i64,
    /// Its kind's word ([`is_word`]).
    pub k: String,
}

impl Envelope {
    /// One line's envelope: `"v":1`, a whole `t` and a `k` word ([`is_word`]); `None` for a line
    /// no schema-1 writer wrote.
    #[must_use]
    pub fn parse(line: &str) -> Option<Envelope> {
        if line.len() > ROW_MAX {
            return None;
        }
        let v: Value = aterm_json::from_str(line).ok()?;
        Envelope::of(v.as_object()?)
    }

    fn of(o: &Map) -> Option<Envelope> {
        if o.get("v").and_then(Value::as_u64) != Some(SCHEMA) {
            return None;
        }
        let k = o.get("k").and_then(Value::as_str).filter(|k| is_word(k))?;
        Some(Envelope {
            t: o.get("t").and_then(Value::as_i64)?,
            k: k.to_owned(),
        })
    }
}

/// One row of a series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRow {
    /// When it was written; a `day` row's local midnight; a `close` row's end of its window.
    pub t: i64,
    /// The machine that wrote it.
    pub by: String,
    /// What it says.
    pub said: Said,
}

/// The keys a reader knows of one account: its lab key ([`accounts::Account::pk`]) and its
/// account key ([`accounts::Account::key`]), either or both. A `fig`, `close` or `day` row carries
/// the keys its record held ([`SeriesRow::keys`]), which tell one account's rows from another's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys<'a> {
    /// The lab key (`ac-…`).
    pub ac: Option<&'a str>,
    /// The account key (`c-…`).
    pub key: Option<AccountKey>,
}

impl Keys<'_> {
    /// Whether a row carrying `carried` (its lab key, its account key) is another account's: it
    /// carries a key of a kind these hold, and a different one.
    #[must_use]
    pub fn others(&self, (ac, key): (Option<&str>, Option<AccountKey>)) -> bool {
        matches!((self.ac, ac), (Some(mine), Some(theirs)) if mine != theirs)
            || matches!((self.key, key), (Some(mine), Some(theirs)) if mine != theirs)
    }

    /// Whether a row carrying `carried` is this account's: it carries a key these hold, and no
    /// other ([`Keys::others`]).
    #[must_use]
    pub fn names(&self, carried: (Option<&str>, Option<AccountKey>)) -> bool {
        let (ac, key) = carried;
        !self.others(carried)
            && ((self.ac.is_some() && ac == self.ac) || (self.key.is_some() && key == self.key))
    }
}

/// `o[key]` as a bounded word.
fn word(o: &Map, key: &str) -> Option<String> {
    o.get(key)
        .and_then(Value::as_str)
        .map(|s| s.chars().take(accounts::WORD_MAX).collect::<String>())
}

/// `ac` as a lab key (`ac-<16 hex>`, [`super::journal::Key`]); nothing else is ever written.
fn lab_key(ac: Option<&str>) -> Option<&str> {
    ac.filter(|ac| super::journal::Key::new(ac).is_some())
}

fn window_id(text: &str) -> Option<&'static str> {
    match text {
        accounts::SESSION_LIMIT => Some(accounts::SESSION_LIMIT),
        accounts::WEEK_LIMIT => Some(accounts::WEEK_LIMIT),
        _ => None,
    }
}

fn put_keys(o: &mut Map, ac: Option<&String>, key: Option<&AccountKey>) {
    if let Some(ac) = lab_key(ac.map(String::as_str)) {
        o.insert("ac".into(), Value::from(ac));
    }
    if let Some(key) = key {
        o.insert("key".into(), Value::from(key.to_string()));
    }
}

fn keys_of(o: &Map) -> (Option<String>, Option<AccountKey>) {
    (
        lab_key(o.get("ac").and_then(Value::as_str)).map(str::to_owned),
        o.get("key")
            .and_then(Value::as_str)
            .and_then(AccountKey::parse),
    )
}

fn pct(v: Option<&Value>) -> Option<u32> {
    u32::try_from(v?.as_u64()?)
        .ok()
        .filter(|p| *p <= 10 * accounts::PCT_MAX)
}

impl SeriesRow {
    /// The row's figure of `window` ([`accounts::SESSION_LIMIT`], [`accounts::WEEK_LIMIT`]).
    #[must_use]
    pub fn fig(&self, window: &str) -> Option<&Fig> {
        match &self.said {
            Said::Fig {
                five_hour,
                seven_day,
                ..
            } => match window {
                accounts::SESSION_LIMIT => five_hour.as_ref(),
                accounts::WEEK_LIMIT => seven_day.as_ref(),
                _ => None,
            },
            Said::Beat | Said::Plan(_) | Said::Close(_) | Said::Day(_) | Said::Event(_) => None,
        }
    }

    /// The keys the row carries (its record's lab key and account key, either of which may be
    /// absent): a `fig`, `close` or `day` row's; `None` for a kind that carries none (a `beat`
    /// repeats the last `fig` row's).
    #[must_use]
    pub fn keys(&self) -> Option<(Option<&str>, Option<AccountKey>)> {
        match &self.said {
            Said::Fig { ac, key, .. } => Some((ac.as_deref(), *key)),
            Said::Close(c) => Some((c.ac.as_deref(), c.key)),
            Said::Day(d) => Some((d.ac.as_deref(), d.key)),
            Said::Beat | Said::Plan(_) | Said::Event(_) => None,
        }
    }

    /// The row as one line of its file, at most [`ROW_MAX`] bytes; `None` past that.
    #[must_use]
    pub fn line(&self) -> Option<String> {
        let mut o = Map::new();
        o.insert("v".into(), Value::from(SCHEMA));
        o.insert("t".into(), Value::from(self.t));
        let kind = match &self.said {
            Said::Fig {
                five_hour,
                seven_day,
                ac,
                key,
            } => {
                let mut w = Map::new();
                for (name, fig) in [
                    (accounts::SESSION_LIMIT, five_hour),
                    (accounts::WEEK_LIMIT, seven_day),
                ] {
                    if let Some(fig) = fig {
                        let mut f = Map::new();
                        f.insert("pct_x10".into(), Value::from(fig.pct_x10));
                        f.insert("reset".into(), Value::from(fig.reset));
                        f.insert("at".into(), Value::from(fig.at));
                        f.insert("src".into(), Value::from(fig.src.clone()));
                        if let Some(epoch) = fig.epoch {
                            f.insert("epoch".into(), Value::from(epoch));
                        }
                        w.insert(name.into(), Value::Object(f));
                    }
                }
                o.insert("w".into(), Value::Object(w));
                put_keys(&mut o, ac.as_ref(), key.as_ref());
                "fig"
            }
            Said::Beat => "beat",
            Said::Plan(plan) => {
                let mut p = Map::new();
                p.insert("word".into(), Value::from(plan.word.clone()));
                if let Some(tier) = &plan.tier {
                    p.insert("tier".into(), Value::from(tier.clone()));
                }
                p.insert("src".into(), Value::from(plan.src.clone()));
                o.insert("plan".into(), Value::Object(p));
                "plan"
            }
            Said::Close(c) => {
                o.insert("w".into(), Value::from(c.w));
                o.insert("reset".into(), Value::from(c.reset));
                o.insert("from".into(), Value::from(c.from));
                o.insert("peak".into(), Value::from(c.peak));
                o.insert("end".into(), Value::from(c.end));
                o.insert("first".into(), Value::from(c.first));
                o.insert("last".into(), Value::from(c.last));
                o.insert("n".into(), Value::from(c.n));
                o.insert("walled".into(), Value::from(c.walled));
                o.insert(
                    "epochs".into(),
                    Value::Array(c.epochs.iter().map(|e| Value::from(*e)).collect()),
                );
                o.insert("early".into(), Value::from(c.early));
                o.insert("src".into(), Value::from(c.src.clone()));
                put_keys(&mut o, c.ac.as_ref(), c.key.as_ref());
                "close"
            }
            Said::Day(d) => {
                o.insert("span_s".into(), Value::from(d.span_s));
                o.insert("tz_s".into(), Value::from(d.tz_s));
                let mut w = Map::new();
                if let Some(five) = d.five_hour {
                    let mut f = Map::new();
                    f.insert("closed".into(), Value::from(five.closed));
                    f.insert("peak".into(), Value::from(five.peak));
                    f.insert("walled".into(), Value::from(five.walled));
                    w.insert(accounts::SESSION_LIMIT.into(), Value::Object(f));
                }
                if let Some(week) = d.seven_day {
                    let mut f = Map::new();
                    f.insert("first".into(), Value::from(week.first));
                    f.insert("last".into(), Value::from(week.last));
                    f.insert("reset".into(), Value::from(week.reset));
                    f.insert("closed".into(), Value::from(week.closed));
                    w.insert(accounts::WEEK_LIMIT.into(), Value::Object(f));
                }
                o.insert("w".into(), Value::Object(w));
                o.insert("n".into(), Value::from(d.n));
                put_keys(&mut o, d.ac.as_ref(), d.key.as_ref());
                "day"
            }
            Said::Event(e) => {
                o.insert("e".into(), Value::from(e.e.as_str()));
                for (key, word) in [("w", &e.w), ("kind", &e.kind)] {
                    if let Some(word) = word {
                        o.insert(key.into(), Value::from(word.clone()));
                    }
                }
                for (key, number) in [("at", e.at), ("reset", e.reset), ("use_by", e.use_by)] {
                    if let Some(number) = number {
                        o.insert(key.into(), Value::from(number));
                    }
                }
                if let Some(n) = e.n {
                    o.insert("n".into(), Value::from(n));
                }
                "event"
            }
        };
        o.insert("k".into(), Value::from(kind));
        o.insert("by".into(), Value::from(self.by.clone()));
        let line = aterm_json::to_string(&Value::Object(o)).ok()?;
        (line.len() <= ROW_MAX).then_some(line)
    }

    /// One line of a series file; `None` for a line it cannot read, a row of a kind this build
    /// does not know included ([`Envelope`] reads those).
    #[must_use]
    pub fn parse(line: &str) -> Option<SeriesRow> {
        if line.len() > ROW_MAX {
            return None;
        }
        let v: Value = aterm_json::from_str(line).ok()?;
        SeriesRow::of(v.as_object()?)
    }

    fn of(o: &Map) -> Option<SeriesRow> {
        let Envelope { t, k } = Envelope::of(o)?;
        let by = o.get("by").and_then(Value::as_str).unwrap_or("").to_owned();
        let said = match k.as_str() {
            "fig" => {
                let w = o.get("w").and_then(Value::as_object)?;
                let fig = |name: &str| {
                    let f = w.get(name)?.as_object()?;
                    Some(Fig {
                        pct_x10: pct(f.get("pct_x10"))?,
                        reset: f.get("reset")?.as_i64()?,
                        at: f.get("at")?.as_i64()?,
                        src: word(f, "src").unwrap_or_default(),
                        epoch: f.get("epoch").and_then(Value::as_i64),
                    })
                };
                let (ac, key) = keys_of(o);
                Said::Fig {
                    five_hour: fig(accounts::SESSION_LIMIT),
                    seven_day: fig(accounts::WEEK_LIMIT),
                    ac,
                    key,
                }
            }
            "beat" => Said::Beat,
            "plan" => {
                let p = o.get("plan").and_then(Value::as_object)?;
                Said::Plan(PlanSaid {
                    word: word(p, "word")?,
                    tier: word(p, "tier"),
                    src: word(p, "src").unwrap_or_default(),
                })
            }
            "close" => {
                let int = |key: &str| o.get(key).and_then(Value::as_i64);
                let (ac, key) = keys_of(o);
                Said::Close(Close {
                    w: window_id(o.get("w")?.as_str()?)?,
                    reset: int("reset")?,
                    from: int("from")?,
                    peak: pct(o.get("peak"))?,
                    end: pct(o.get("end"))?,
                    first: int("first")?,
                    last: int("last")?,
                    n: u32::try_from(o.get("n")?.as_u64()?).ok()?,
                    walled: o.get("walled")?.as_bool()?,
                    epochs: o
                        .get("epochs")?
                        .as_array()?
                        .iter()
                        .take(EPOCHS_MAX)
                        .map(Value::as_i64)
                        .collect::<Option<Vec<i64>>>()?,
                    early: o.get("early")?.as_bool()?,
                    src: word(o, "src").unwrap_or_default(),
                    ac,
                    key,
                })
            }
            "day" => {
                let w = o.get("w").and_then(Value::as_object)?;
                let count = |f: &Map, key: &str| u32::try_from(f.get(key)?.as_u64()?).ok();
                let five_hour = w
                    .get(accounts::SESSION_LIMIT)
                    .and_then(Value::as_object)
                    .and_then(|f| {
                        Some(DayFive {
                            closed: count(f, "closed")?,
                            peak: pct(f.get("peak"))?,
                            walled: count(f, "walled")?,
                        })
                    });
                let seven_day = w
                    .get(accounts::WEEK_LIMIT)
                    .and_then(Value::as_object)
                    .and_then(|f| {
                        Some(DayWeek {
                            first: pct(f.get("first"))?,
                            last: pct(f.get("last"))?,
                            reset: f.get("reset")?.as_i64()?,
                            closed: f.get("closed")?.as_bool()?,
                        })
                    });
                let (ac, key) = keys_of(o);
                Said::Day(Day {
                    span_s: o.get("span_s")?.as_i64()?,
                    tz_s: o.get("tz_s")?.as_i64()?,
                    five_hour,
                    seven_day,
                    n: count(o, "n")?,
                    ac,
                    key,
                })
            }
            "event" => Said::Event(EventSaid {
                e: EventWord::parse(o.get("e")?.as_str()?)?,
                w: word(o, "w"),
                at: o.get("at").and_then(Value::as_i64),
                reset: o.get("reset").and_then(Value::as_i64),
                kind: word(o, "kind"),
                n: o.get("n")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                use_by: o.get("use_by").and_then(Value::as_i64),
            }),
            _ => return None,
        };
        Some(SeriesRow { t, by, said })
    }
}

/// The series of `label` on `machine`: `u-<label>@<machine>`. A renamed label starts a new one;
/// its rows' keys say which account it was.
#[must_use]
pub fn series_name(label: &str, machine: &str) -> String {
    format!("u-{label}@{machine}")
}

/// The series directory of the accounts store under `harness_state`.
#[must_use]
pub fn dir_in(harness_state: &Path) -> PathBuf {
    accounts::Store::at(harness_state)
        .state_file()
        .parent()
        .map_or_else(|| harness_state.join("accounts"), Path::to_path_buf)
        .join(DIR)
}

/// The file of `label`'s series on `machine` under `harness_state`; `None` for a label or a
/// machine id no file may be named after.
#[must_use]
pub fn path_of(harness_state: &Path, label: &str, machine: &str) -> Option<PathBuf> {
    let machine_ok = !machine.is_empty()
        && machine.len() <= accounts::WORD_MAX
        && machine.bytes().all(|b| b.is_ascii_alphanumeric());
    (accounts::admissible_label(label) && machine_ok)
        .then(|| dir_in(harness_state).join(format!("{}.jsonl", series_name(label, machine))))
}

/// What a writer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noted {
    /// A `fig` row was appended.
    Fig,
    /// A `beat` row was appended.
    Beat,
    /// Only an `event` row was appended.
    Event,
    /// A `plan` row was appended.
    Plan,
    /// The plan equals the series' last `plan` row: nothing was written.
    SamePlan,
    /// The series' lock stayed held past [`LOCK_WAIT`]: a `fig` or `beat` row was dropped (the
    /// next reading comes within `READING_HEARTBEAT_S`), and an `event` or `plan` row waits in
    /// this process's outbox for the next append to the file.
    Dropped,
    /// The label or machine names no file, or there was nothing to write.
    Nothing,
}

/// The held [`LOCK_FILE`]; dropping it releases the lock.
struct SeriesLock(std::fs::File);

impl Drop for SeriesLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn open_private(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut open = std::fs::OpenOptions::new();
    open.create(true);
    if append {
        open.append(true);
    } else {
        open.write(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file: std::fs::File = aterm_uds::kept_out(|| open.open(path))?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: not a regular file", path.display()),
        ));
    }
    Ok(file)
}

fn lock_within(dir: &Path, wait: Duration) -> std::io::Result<Option<SeriesLock>> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let file = open_private(&dir.join(LOCK_FILE), false)?;
    let step = Duration::from_millis(10);
    let mut waited = Duration::ZERO;
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: SeriesLock's Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => return Ok(Some(SeriesLock(file))),
            Err(std::fs::TryLockError::WouldBlock) if waited < wait => {
                std::thread::sleep(step);
                waited += step;
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// The last `fig` row in the end of the file at `path`, looking back [`LAST_FIG_BYTES`].
fn last_fig(path: &Path) -> Option<SeriesRow> {
    let (mut file, meta) = super::footer::open_regular_with_meta(path)?;
    let len = meta.len();
    let from = len.saturating_sub(LAST_FIG_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;
    let cap = usize::try_from(
        len.saturating_sub(from)
            .min(LAST_FIG_BYTES)
            .saturating_add(1),
    )
    .unwrap_or(0);
    let mut text = String::with_capacity(cap);
    file.take(LAST_FIG_BYTES).read_to_string(&mut text).ok()?;
    text.lines()
        .rev()
        .filter_map(SeriesRow::parse)
        .find(|row| matches!(row.said, Said::Fig { .. }))
}

/// Appends `rows` to the file at `path` in one write; the file's length after it. The caller
/// holds [`LOCK_FILE`].
fn append_locked(path: &Path, rows: &[SeriesRow]) -> std::io::Result<u64> {
    let mut bytes = String::new();
    for line in rows.iter().filter_map(SeriesRow::line) {
        bytes.push_str(&line);
        bytes.push('\n');
    }
    let mut file = open_private(path, true)?;
    if !bytes.is_empty() {
        file.write_all(bytes.as_bytes())?;
    }
    Ok(file.metadata()?.len())
}

/// The machine's own zone at `t`, seconds east of UTC (UTC where it cannot be read): what an
/// append's compaction keeps its local days by.
fn live_zone(t: i64) -> i64 {
    crate::supervise::limit::local_offset_at(t).unwrap_or(0)
}

/// The `event` and `plan` rows a writer of this process could not append because the series'
/// lock stayed held past its wait, by file. The next writer to that file to take the lock appends
/// them first, at their own instants. A `fig` row needs no outbox: the next reading or the
/// reading heartbeat writes the figures again, where an event or a tier would not come again. A
/// crash between the store's commit and the append loses what it holds.
static OUTBOX: std::sync::Mutex<BTreeMap<PathBuf, Vec<SeriesRow>>> =
    std::sync::Mutex::new(BTreeMap::new());

/// Holds `rows` for `path`'s next append: a `plan` row in place of any it held, and at most
/// [`OUTBOX_MAX`] rows, the newest.
fn outbox_put(path: &Path, rows: Vec<SeriesRow>) {
    if rows.is_empty() {
        return;
    }
    let mut held = OUTBOX.lock_or_recover();
    let pending = held.entry(path.to_path_buf()).or_default();
    for row in rows {
        if matches!(row.said, Said::Plan(_)) {
            pending.retain(|r| !matches!(r.said, Said::Plan(_)));
        }
        pending.push(row);
    }
    let over = pending.len().saturating_sub(OUTBOX_MAX);
    pending.drain(..over);
}

fn outbox_holds(path: &Path) -> bool {
    OUTBOX.lock_or_recover().contains_key(path)
}

/// The rows the outbox holds for `path`, taken out of it; a `plan` row the file's newest `plan`
/// row already says is left out. The caller holds [`LOCK_FILE`].
fn outbox_take_locked(path: &Path) -> Vec<SeriesRow> {
    let mut rows = OUTBOX.lock_or_recover().remove(path).unwrap_or_default();
    if rows.iter().any(|r| matches!(r.said, Said::Plan(_))) {
        let newest = newest_plan(&read_path(path)).cloned();
        rows.retain(|r| match &r.said {
            Said::Plan(plan) => !newest.as_ref().is_some_and(|n| n.says(plan)),
            _ => true,
        });
    }
    rows
}

/// A recorded reading's windows and the record's keys, as a writer takes them.
type Reading = (
    (Option<Fig>, Option<Fig>),
    (Option<String>, Option<AccountKey>),
);

/// Appends to `label`'s series on `machine` under `harness_state`, waiting at most `wait` for
/// the series lock: the rows the outbox holds for the file, then an `event` row of each of
/// `events`, then `reading`'s `fig` row, or a `beat` where its figures and keys equal the last
/// `fig` row's. Where the lock stays held, the event rows go to the outbox. An append that takes the
/// file past [`BOUND_BYTES`] compacts it at once, under the same lock; then, the lock released,
/// a pass over every series runs where one is due and this append claims it ([`compact_due`]).
/// A compaction that fails waits for the next append: the rows are written.
fn note_rows(
    harness_state: &Path,
    label: &str,
    machine: &str,
    (events, reading): (&[EventSaid], Option<Reading>),
    now: i64,
    wait: Duration,
) -> std::io::Result<Noted> {
    let Some(path) = path_of(harness_state, label, machine) else {
        return Ok(Noted::Nothing);
    };
    let reading =
        reading.filter(|((five_hour, seven_day), _)| five_hour.is_some() || seven_day.is_some());
    if events.is_empty() && reading.is_none() && !outbox_holds(&path) {
        return Ok(Noted::Nothing);
    }
    let dir = dir_in(harness_state);
    let event_rows: Vec<SeriesRow> = events
        .iter()
        .map(|event| SeriesRow {
            t: now,
            by: machine.to_owned(),
            said: Said::Event(event.clone()),
        })
        .collect();
    let noted = {
        let Some(_lock) = lock_within(&dir, wait)? else {
            outbox_put(&path, event_rows);
            return Ok(Noted::Dropped);
        };
        let mut rows = outbox_take_locked(&path);
        let mut noted = match rows.first() {
            _ if !events.is_empty() => Noted::Event,
            Some(_) if rows.iter().any(|r| matches!(r.said, Said::Event(_))) => Noted::Event,
            Some(_) => Noted::Plan,
            None => Noted::Nothing,
        };
        rows.extend(event_rows);
        if let Some(((five_hour, seven_day), (ac, key))) = reading {
            let ac = lab_key(ac.as_deref()).map(str::to_owned);
            let said = Said::Fig {
                five_hour,
                seven_day,
                ac,
                key,
            };
            let same = last_fig(&path).is_some_and(|last| same_reading(&last.said, &said));
            let (said, kind) = if same {
                (Said::Beat, Noted::Beat)
            } else {
                (said, Noted::Fig)
            };
            rows.push(SeriesRow {
                t: now,
                by: machine.to_owned(),
                said,
            });
            noted = kind;
        }
        if append_locked(&path, &rows)? > BOUND_BYTES {
            let _ = compact_locked(&path, now, live_zone);
        }
        noted
    };
    if compact_due(&dir, now) && claim_pass(&dir, now) {
        let _ = pass_over(&dir, now, live_zone);
    }
    Ok(noted)
}

/// Whether two `fig` rows say the same: equal figures of each window and equal keys.
fn same_reading(a: &Said, b: &Said) -> bool {
    let (
        Said::Fig {
            five_hour: a5,
            seven_day: a7,
            ac: a_ac,
            key: a_key,
        },
        Said::Fig {
            five_hour: b5,
            seven_day: b7,
            ac: b_ac,
            key: b_key,
        },
    ) = (a, b)
    else {
        return false;
    };
    let same = |a: &Option<Fig>, b: &Option<Fig>| match (a, b) {
        (Some(a), Some(b)) => a.same_figure(b),
        (None, None) => true,
        _ => false,
    };
    same(a5, b5) && same(a7, b7) && a_ac == b_ac && a_key == b_key
}

/// Appends `label`'s reading on `machine` to its series under `harness_state`, keyed by
/// nothing: a `fig` row of its windows, or a `beat` where they equal the last `fig` row's. The
/// host writes through [`note_committed`]; this is the seed a lab or a test writes as it.
/// # Errors
/// The directory, the lock or the file could not be made or written.
pub fn note(
    harness_state: &Path,
    label: &str,
    machine: &str,
    five_hour: Option<&WindowFig>,
    seven_day: Option<&WindowFig>,
    now: i64,
) -> std::io::Result<Noted> {
    note_within(
        harness_state,
        label,
        machine,
        (five_hour.map(Fig::of), seven_day.map(Fig::of)),
        (None, None),
        now,
        LOCK_WAIT,
    )
}

/// [`note`] of series figures and the record's keys (`ac`, `key`), waiting at most `wait` for
/// the series lock.
pub(crate) fn note_within(
    harness_state: &Path,
    label: &str,
    machine: &str,
    figs: (Option<Fig>, Option<Fig>),
    keys: (Option<String>, Option<AccountKey>),
    now: i64,
    wait: Duration,
) -> std::io::Result<Noted> {
    note_rows(
        harness_state,
        label,
        machine,
        (&[], Some((figs, keys))),
        now,
        wait,
    )
}

/// What a tap reading's store transaction committed ([`accounts::note_reading_held`]), as the
/// series takes it.
#[derive(Debug, Clone, Copy)]
pub struct Committed<'a> {
    /// The store's five-hour figure of the label after the transaction.
    pub five_hour: Option<&'a WindowFig>,
    /// Its week, likewise.
    pub seven_day: Option<&'a WindowFig>,
    /// The transaction's events.
    pub events: &'a [accounts::Event],
    /// Whether the record held the reading back.
    pub held_back: bool,
    /// The record's lab key ([`accounts::Account::pk`]).
    pub ac: Option<&'a str>,
    /// The record's account key ([`accounts::Account::key`]).
    pub key: Option<AccountKey>,
}

impl Committed<'_> {
    /// Whether the transaction recorded the reading: it gave events and held nothing back. Only
    /// then does the series take a `fig` or `beat` row.
    #[must_use]
    pub fn recorded(&self) -> bool {
        !self.events.is_empty() && !self.held_back
    }
}

/// Appends what a tap reading's transaction committed to `label`'s series on `machine` under
/// `harness_state`, in one locked append: an `another-account` `event` row for each
/// [`accounts::Kind::Epoch`] event, held back or not, since the transaction committed it; then,
/// where the reading was [`Committed::recorded`], a `fig` row of the store's figures and the
/// record's keys, or a `beat`. Called right after the store's transaction, so its cadence is the
/// store's: a whole-percent change or the reading heartbeat.
/// # Errors
/// As [`note`].
pub fn note_committed(
    harness_state: &Path,
    label: &str,
    machine: &str,
    committed: &Committed<'_>,
    now: i64,
) -> std::io::Result<Noted> {
    let events: Vec<EventSaid> = committed
        .events
        .iter()
        .filter(|e| e.kind == accounts::Kind::Epoch)
        .map(|_| EventSaid::of(EventWord::AnotherAccount))
        .collect();
    let reading = committed.recorded().then(|| {
        (
            (
                committed.five_hour.map(Fig::of),
                committed.seven_day.map(Fig::of),
            ),
            (committed.ac.map(str::to_owned), committed.key),
        )
    });
    note_rows(
        harness_state,
        label,
        machine,
        (&events, reading),
        now,
        LOCK_WAIT,
    )
}

/// Appends an `event` row of each of `events` to `label`'s series on `machine` under
/// `harness_state`, after the rows the outbox holds for the file, in one locked append; where the
/// lock stays held past [`LOCK_WAIT`] they wait in the outbox for the next append to the file.
/// Called by the host right after the store transaction that committed them (a model's week at
/// its wall, [`accounts::record_usage`]; a limit-reset offer, [`accounts::note_offer`]), never
/// under `accounts.lock`.
/// # Errors
/// As [`note`].
pub fn note_events(
    harness_state: &Path,
    label: &str,
    machine: &str,
    events: &[EventSaid],
    now: i64,
) -> std::io::Result<Noted> {
    if events.is_empty() {
        return Ok(Noted::Nothing);
    }
    note_rows(
        harness_state,
        label,
        machine,
        (events, None),
        now,
        LOCK_WAIT,
    )
}

/// Appends `plan` to `label`'s series on `machine` under `harness_state`, unless the series'
/// newest `plan` row says the same, after the rows the outbox holds for the file; where the lock
/// stays held the row goes to the outbox (it supersedes a plan held there). It never compacts.
/// # Errors
/// As [`note`].
pub fn note_plan(
    harness_state: &Path,
    label: &str,
    machine: &str,
    plan: &PlanSaid,
    now: i64,
) -> std::io::Result<Noted> {
    let Some(path) = path_of(harness_state, label, machine) else {
        return Ok(Noted::Nothing);
    };
    let row = SeriesRow {
        t: now,
        by: machine.to_owned(),
        said: Said::Plan(plan.clone()),
    };
    let dir = dir_in(harness_state);
    let Some(_lock) = lock_within(&dir, LOCK_WAIT)? else {
        outbox_put(&path, vec![row]);
        return Ok(Noted::Dropped);
    };
    let mut rows: Vec<SeriesRow> = outbox_take_locked(&path)
        .into_iter()
        .filter(|r| !matches!(r.said, Said::Plan(_)))
        .collect();
    let same = newest_plan(&read_path(&path)).is_some_and(|said| said.says(plan));
    if !same {
        rows.push(row);
    }
    if !rows.is_empty() {
        append_locked(&path, &rows)?;
    }
    Ok(if same { Noted::SamePlan } else { Noted::Plan })
}

/// The series' newest `plan` row's plan.
#[must_use]
pub fn newest_plan(rows: &[SeriesRow]) -> Option<&PlanSaid> {
    rows.iter().rev().find_map(|row| match &row.said {
        Said::Plan(plan) => Some(plan),
        _ => None,
    })
}

fn read_path(path: &Path) -> Vec<SeriesRow> {
    read_file(path)
}

/// Every readable row of the series file at `path`, in file order (at most [`READ_MAX`] bytes;
/// nothing where it cannot be read). Readers take no lock.
#[must_use]
pub fn read_file(path: &Path) -> Vec<SeriesRow> {
    accounts::read_bounded(path, READ_MAX)
        .ok()
        .flatten()
        .map(|text| text.lines().filter_map(SeriesRow::parse).collect())
        .unwrap_or_default()
}

/// One schema-1 line of a series file as it is stored ([`stored_rows`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// Its `t`.
    pub t: i64,
    /// The line exactly as stored.
    pub line: String,
    /// What it says, where this build reads its kind ([`SeriesRow::parse`]).
    pub row: Option<SeriesRow>,
}

/// Every schema-1 line of the series file at `path` exactly as it is stored, in file order, each
/// read once: a row of a kind this build does not know included ([`Envelope`]), with no row. At
/// most [`READ_MAX`] bytes; nothing where it cannot be read.
#[must_use]
pub fn stored_rows(path: &Path) -> Vec<Stored> {
    accounts::read_bounded(path, READ_MAX)
        .ok()
        .flatten()
        .map(|text| {
            text.lines()
                .filter(|line| line.len() <= ROW_MAX)
                .filter_map(|line| {
                    let v: Value = aterm_json::from_str(line).ok()?;
                    let o = v.as_object()?;
                    Some(Stored {
                        t: Envelope::of(o)?.t,
                        line: line.to_owned(),
                        row: SeriesRow::of(o),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the series file at `path` is one of `machine`'s (`u-<label>@<machine>.jsonl`,
/// [`series_name`]): the rows a machine read itself.
#[must_use]
pub fn is_of_machine(path: &Path, machine: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".jsonl"))
        .and_then(|stem| stem.rsplit_once('@'))
        .is_some_and(|(_, of)| of == machine)
}

/// Every series file in the series directory `dir` (`u-*.jsonl`, regular files), sorted by
/// path; none where the directory does not exist.
/// # Errors
/// The directory could not be listed.
pub fn series_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("u-")
                && name.ends_with(".jsonl")
                && entry.file_type().is_ok_and(|t| t.is_file())
        })
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    Ok(paths)
}

/// Every readable row of `label`'s series on `machine` under `harness_state`, in file order
/// (oldest first). Readers take no lock.
#[must_use]
pub fn read(harness_state: &Path, label: &str, machine: &str) -> Vec<SeriesRow> {
    path_of(harness_state, label, machine)
        .map(|path| read_path(&path))
        .unwrap_or_default()
}

/// Whether the outlook reads `row` at `now`: a `fig` or `beat` row inside [`VIEW_S`] before
/// `now`, or a `plan` row.
fn in_outlook_view(row: &SeriesRow, now: i64) -> bool {
    match row.said {
        Said::Plan(_) => true,
        Said::Fig { .. } | Said::Beat => now.saturating_sub(row.t) <= VIEW_S,
        Said::Close(_) | Said::Day(_) | Said::Event(_) => false,
    }
}

/// The rows the outlook reads of a series ([`read`]) at `now`: its `fig` and `beat` rows inside
/// [`VIEW_S`] before `now` (a row's week reading repeats its `fig` at each beat, [`Said::Beat`])
/// and every `plan` row, in file order. A row a clock ahead wrote is held too and
/// hides no other: the outlook's own admission refuses its figures
/// ([`super::profile::usage_read_age`]). A [`Tail`] holds the same, so the card and the CLI read
/// one set of rows however long the file's history.
#[must_use]
pub fn outlook_view(rows: Vec<SeriesRow>, now: i64) -> Vec<SeriesRow> {
    rows.into_iter()
        .filter(|row| in_outlook_view(row, now))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Runs, closes and days

/// How a run began, which says where its rise starts when no row precedes a lookback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Began {
    /// A window's first run: empty at `reset − W`.
    Window,
    /// After a limit reset used inside the window: empty at the epoch.
    Epoch(i64),
    /// After a refill: at its first row.
    Refill,
}

/// One run of one window: figures of one reset, no refill and no epoch cut between them.
#[derive(Clone, Debug)]
pub struct Run<'a> {
    /// How it began.
    pub began: Began,
    /// Its figures, by when they were served.
    pub figs: Vec<&'a Fig>,
}

/// `figs` of one window split into runs, by when each was served: a new reset begins a run
/// ([`Began::Window`]), and so do an epoch after the previous figure ([`Began::Epoch`]) and a
/// drop of at least [`accounts::REFILL_MIN_X10`] under one reset ([`Began::Refill`]).
#[must_use]
pub fn runs<'a>(figs: impl IntoIterator<Item = &'a Fig>) -> Vec<Run<'a>> {
    let mut figs: Vec<&Fig> = figs.into_iter().collect();
    figs.sort_by_key(|f| f.at);
    let mut out: Vec<Run<'a>> = Vec::new();
    for fig in figs {
        let began = match out.last().and_then(|run| run.figs.last()) {
            None => Some(Began::Window),
            Some(prev) if prev.reset != fig.reset => Some(Began::Window),
            Some(prev) => match fig.epoch.filter(|e| *e > prev.at && *e <= fig.at) {
                Some(epoch) => Some(Began::Epoch(epoch)),
                None => (fig.pct_x10.saturating_add(accounts::REFILL_MIN_X10) <= prev.pct_x10)
                    .then_some(Began::Refill),
            },
        };
        match (began, out.last_mut()) {
            (None, Some(run)) => run.figs.push(fig),
            (began, _) => out.push(Run {
                began: began.unwrap_or(Began::Refill),
                figs: vec![fig],
            }),
        }
    }
    out
}

/// The newest of each `(window, reset)`'s `fig` rows' keys and machine.
type AnchorKeys<'a> =
    BTreeMap<(&'static str, i64), (Option<&'a String>, Option<AccountKey>, &'a str)>;

fn anchor_keys(rows: &[SeriesRow]) -> AnchorKeys<'_> {
    let mut out: AnchorKeys<'_> = BTreeMap::new();
    let mut ordered: Vec<&SeriesRow> = rows.iter().collect();
    ordered.sort_by_key(|r| r.t);
    for row in ordered {
        let Said::Fig { ac, key, .. } = &row.said else {
            continue;
        };
        for w in [accounts::SESSION_LIMIT, accounts::WEEK_LIMIT] {
            if let Some(fig) = row.fig(w) {
                let held = out.entry((w, fig.reset)).or_insert((None, None, ""));
                held.0 = ac.as_ref().or(held.0);
                held.1 = key.or(held.1);
                held.2 = &row.by;
            }
        }
    }
    out
}

/// The `close` rows `rows` hold no row for yet, one per window anchor that ended by `now`, of
/// the figures the display admits at `now` ([`super::profile::usage_read_age`]: one a clock ahead
/// served names no anchor yet). An anchor ends [`CLOSE_SLACK_S`] after its reset, or at the first
/// figure of the run read after its last ([`runs`], by when each was served, so another account
/// signed in to the home ends the one before it whichever window resets first), and came
/// `early` where that figure came before its reset. An anchor whose figures all came before the
/// newest close's last figure is never closed again, so a close a bound gave up is not made again
/// from what is left. Each row's `t` is when its window ended, and it inherits the keys of its
/// anchor's newest `fig` row. Sorted by `t`.
#[must_use]
pub fn derive_closes(rows: &[SeriesRow], now: i64) -> Vec<SeriesRow> {
    let keys = anchor_keys(rows);
    let mut out = Vec::new();
    for (w, len) in [
        (accounts::SESSION_LIMIT, accounts::SESSION_WINDOW_S),
        (accounts::WEEK_LIMIT, accounts::WEEK_WINDOW_S),
    ] {
        let held: Vec<&Close> = rows
            .iter()
            .filter_map(|r| match &r.said {
                Said::Close(c) if c.w == w => Some(c),
                _ => None,
            })
            .collect();
        let closed: BTreeSet<i64> = held.iter().map(|c| c.reset).collect();
        let closed_through = held.iter().map(|c| c.last).max();
        let split = runs(
            rows.iter()
                .filter_map(|r| r.fig(w))
                .filter(|f| super::profile::usage_read_age(f.at, now).is_some()),
        );
        // Each anchor's runs, and where in `split` its last one is.
        let mut anchors: BTreeMap<i64, (Vec<&Run<'_>>, usize)> = BTreeMap::new();
        for (i, run) in split.iter().enumerate() {
            if let Some(first) = run.figs.first() {
                let (its, last) = anchors.entry(first.reset).or_insert((Vec::new(), i));
                its.push(run);
                *last = i;
            }
        }
        for (reset, (its, last_run)) in &anchors {
            if closed.contains(reset) {
                continue;
            }
            let figs: Vec<&Fig> = its
                .iter()
                .flat_map(|run| run.figs.iter().copied())
                .collect();
            let (Some(first), Some(last)) = (figs.first(), figs.last()) else {
                continue;
            };
            if closed_through.is_some_and(|through| last.at <= through) {
                continue;
            }
            let newer = split
                .get(last_run + 1)
                .and_then(|run| run.figs.first())
                .map(|f| f.at);
            if newer.is_none() && now < reset.saturating_add(CLOSE_SLACK_S) {
                continue;
            }
            let from = reset - len;
            let mut epochs: BTreeSet<i64> = figs
                .iter()
                .filter_map(|f| f.epoch)
                .filter(|e| *e > from && *e <= *reset)
                .collect();
            for run in its {
                match run.began {
                    Began::Epoch(at) => {
                        epochs.insert(at);
                    }
                    Began::Refill => {
                        if let Some(f) = run.figs.first() {
                            epochs.insert(f.at);
                        }
                    }
                    Began::Window => {}
                }
            }
            let epochs: Vec<i64> = epochs.into_iter().collect();
            let epochs = epochs[epochs.len().saturating_sub(EPOCHS_MAX)..].to_vec();
            let peak = figs.iter().map(|f| f.pct_x10).max().unwrap_or(0);
            let early = newer.is_some_and(|at| at < *reset);
            let ended = newer.map_or(*reset, |at| at.min(*reset)).max(last.at);
            let (ac, key, by) = keys.get(&(w, *reset)).copied().unwrap_or((None, None, ""));
            out.push(SeriesRow {
                t: ended,
                by: by.to_owned(),
                said: Said::Close(Close {
                    w,
                    reset: *reset,
                    from,
                    peak,
                    end: last.pct_x10,
                    first: first.at,
                    last: last.at,
                    n: u32::try_from(figs.len()).unwrap_or(u32::MAX),
                    walled: peak >= 1000,
                    epochs,
                    early,
                    src: SRC_SERIES.into(),
                    ac: ac.cloned(),
                    key,
                }),
            });
        }
    }
    out.sort_by_key(|r| r.t);
    out
}

/// The local day `t` falls on under `tz_at` (seconds east of UTC at an instant): a day number.
#[must_use]
pub fn local_day(t: i64, tz_at: fn(i64) -> i64) -> i64 {
    t.saturating_add(tz_at(t)).div_euclid(DAY_S)
}

/// The instant local day `day` begins under `tz_at`: its local midnight, or where a zone change
/// moved the clocks forward across that midnight (a zone that springs forward at 00:00), the
/// day's first instant, the change. A day the clocks skipped whole begins where the next one
/// does.
#[must_use]
pub fn midnight(day: i64, tz_at: fn(i64) -> i64) -> i64 {
    let local = day.saturating_mul(DAY_S);
    let guess = local.saturating_sub(tz_at(local));
    let t = local.saturating_sub(tz_at(guess));
    if local_day(t, tz_at) >= day {
        return t;
    }
    // `t` is the midnight the old offset would give, still on the day before: step an hour at a
    // time to an instant on the day (no zone moves its clocks by more than a day), then halve the
    // last hour, which holds the change, down to its second.
    let (mut before, mut on) = (t, t.saturating_add(3600));
    for _ in 0..48 {
        if local_day(on, tz_at) >= day {
            break;
        }
        before = on;
        on = on.saturating_add(3600);
    }
    while on.saturating_sub(before) > 1 {
        let mid = before.saturating_add(on.saturating_sub(before) / 2);
        if local_day(mid, tz_at) >= day {
            on = mid;
        } else {
            before = mid;
        }
    }
    on
}

/// What one local day's rows add up to, as [`day_summaries`] reads them.
#[derive(Default)]
struct DayAcc<'a> {
    n: u32,
    five_fig_peak: Option<u32>,
    five_close_peak: Option<u32>,
    five_closed: u32,
    five_walled: u32,
    five_any: bool,
    week_first: Option<(i64, u32, i64)>,
    week_last: Option<(i64, u32, i64)>,
    week_close: Option<(i64, u32, i64)>,
    week_closed: bool,
    ac: Option<(i64, &'a String)>,
    key: Option<(i64, AccountKey)>,
    by: Option<(i64, &'a str)>,
}

impl<'a> DayAcc<'a> {
    fn keyed(&mut self, t: i64, by: &'a str, ac: Option<&'a String>, key: Option<AccountKey>) {
        if self.by.is_none_or(|(at, _)| t >= at) {
            self.by = Some((t, by));
        }
        if let Some(ac) = ac
            && self.ac.is_none_or(|(at, _)| t >= at)
        {
            self.ac = Some((t, ac));
        }
        if let Some(key) = key
            && self.key.is_none_or(|(at, _)| t >= at)
        {
            self.key = Some((t, key));
        }
    }

    fn week(&mut self, t: i64, pct_x10: u32, reset: i64) {
        if self.week_first.is_none_or(|(at, _, _)| t < at) {
            self.week_first = Some((t, pct_x10, reset));
        }
        if self.week_last.is_none_or(|(at, _, _)| t >= at) {
            self.week_last = Some((t, pct_x10, reset));
        }
    }
}

/// One `day` row for each local day under `tz_at` that overlaps `[from, to)` and holds a
/// reading or a close, each from all of that day's rows taken in time order whatever order they
/// come in: the readings recorded (`fig` and `beat` rows, a beat repeating the figures and keys
/// of the last `fig` row before it), the five-hour windows closed, their peak and how many read
/// full, and the week's first and last figures, its reset and whether it closed (a day that read
/// no weekly figure takes its last weekly close's end). A day's `t` is its local midnight, its
/// `span_s` its length (23 or 25 hours across a daylight-saving change) and its `tz_s` the
/// zone's offset then. The one definition of a day: compaction writes these, and a history reads
/// them for the days it holds rows of.
#[must_use]
pub fn day_summaries(
    rows: &[SeriesRow],
    from: i64,
    to: i64,
    tz_at: fn(i64) -> i64,
) -> Vec<SeriesRow> {
    let (lo, hi) = (from.saturating_sub(2 * DAY_S), to.saturating_add(2 * DAY_S));
    let mut ordered: Vec<&SeriesRow> = rows.iter().collect();
    ordered.sort_by_key(|r| r.t);
    let mut days: BTreeMap<i64, DayAcc<'_>> = BTreeMap::new();
    let mut last_fig: Option<&Said> = None;
    for row in ordered {
        let said = match &row.said {
            Said::Fig { .. } => {
                last_fig = Some(&row.said);
                &row.said
            }
            Said::Beat => last_fig.unwrap_or(&row.said),
            other => other,
        };
        let counts = matches!(row.said, Said::Fig { .. } | Said::Beat | Said::Close(_));
        if !counts || row.t < lo || row.t >= hi {
            continue;
        }
        let acc = days.entry(local_day(row.t, tz_at)).or_default();
        match said {
            Said::Fig {
                five_hour,
                seven_day,
                ac,
                key,
            } => {
                acc.n = acc.n.saturating_add(1);
                if let Some(f) = five_hour {
                    acc.five_any = true;
                    acc.five_fig_peak =
                        Some(acc.five_fig_peak.map_or(f.pct_x10, |p| p.max(f.pct_x10)));
                }
                if let Some(f) = seven_day {
                    acc.week(row.t, f.pct_x10, f.reset);
                }
                acc.keyed(row.t, &row.by, ac.as_ref(), *key);
            }
            Said::Beat => {
                acc.n = acc.n.saturating_add(1);
                acc.keyed(row.t, &row.by, None, None);
            }
            Said::Close(c) if c.w == accounts::SESSION_LIMIT => {
                acc.five_any = true;
                acc.five_closed = acc.five_closed.saturating_add(1);
                acc.five_walled = acc.five_walled.saturating_add(u32::from(c.walled));
                acc.five_close_peak = Some(acc.five_close_peak.map_or(c.peak, |p| p.max(c.peak)));
                acc.keyed(row.t, &row.by, c.ac.as_ref(), c.key);
            }
            Said::Close(c) => {
                acc.week_closed = true;
                acc.week_close = Some((row.t, c.end, c.reset));
                acc.keyed(row.t, &row.by, c.ac.as_ref(), c.key);
            }
            Said::Plan(_) | Said::Day(_) | Said::Event(_) => {}
        }
    }
    days.into_iter()
        .filter_map(|(day, acc)| {
            let (start, end) = (midnight(day, tz_at), midnight(day.saturating_add(1), tz_at));
            let week_first = acc.week_first.or(acc.week_close);
            (start < to && end > from).then(|| SeriesRow {
                t: start,
                by: acc.by.map(|(_, by)| by.to_owned()).unwrap_or_default(),
                said: Said::Day(Day {
                    span_s: end - start,
                    tz_s: tz_at(start),
                    five_hour: acc.five_any.then(|| DayFive {
                        closed: acc.five_closed,
                        peak: acc.five_fig_peak.or(acc.five_close_peak).unwrap_or(0),
                        walled: acc.five_walled,
                    }),
                    seven_day: acc
                        .week_last
                        .or(acc.week_close)
                        .map(|(_, last, reset)| DayWeek {
                            first: week_first.map_or(last, |(_, first, _)| first),
                            last,
                            reset,
                            closed: acc.week_closed,
                        }),
                    n: acc.n,
                    ac: acc.ac.map(|(_, ac)| ac.clone()),
                    key: acc.key.map(|(_, key)| key),
                }),
            })
        })
        .collect()
}

/// The limit resets used inside `window`'s anchors in `rows` by `now`, oldest first and each
/// once: every `close` row's `epochs`, a stored one's or one [`derive_closes`] gives an anchor
/// that ended, and for an anchor no close holds yet, its figures' `epoch` inside it and each run
/// an epoch or a refill began ([`runs`]), as its close will name them.
#[must_use]
pub fn resets_used(rows: &[SeriesRow], window: &str, now: i64) -> Vec<i64> {
    let derived = derive_closes(rows, now);
    let closes: Vec<&Close> = rows
        .iter()
        .chain(&derived)
        .filter_map(|r| match &r.said {
            Said::Close(c) if c.w == window => Some(c),
            _ => None,
        })
        .collect();
    let closed: BTreeSet<i64> = closes.iter().map(|c| c.reset).collect();
    let mut out: BTreeSet<i64> = closes
        .iter()
        .flat_map(|c| c.epochs.iter().copied())
        .collect();
    let length = match window {
        accounts::SESSION_LIMIT => accounts::SESSION_WINDOW_S,
        _ => accounts::WEEK_WINDOW_S,
    };
    let open: Vec<&Fig> = rows
        .iter()
        .filter_map(|r| r.fig(window))
        .filter(|f| !closed.contains(&f.reset) && f.at <= now)
        .collect();
    out.extend(open.iter().filter_map(|f| {
        f.epoch
            .filter(|e| *e > f.reset.saturating_sub(length) && *e <= f.reset)
    }));
    for run in runs(open.iter().copied()) {
        match run.began {
            Began::Epoch(at) => {
                out.insert(at);
            }
            Began::Refill => out.extend(run.figs.first().map(|f| f.at)),
            Began::Window => {}
        }
    }
    out.into_iter().filter(|t| *t <= now).collect()
}

// ---------------------------------------------------------------------------------------------
// Since

/// What a window did over a span ([`Since`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// It rose by this many whole points; never negative.
    Rose(u32),
    /// The window its figure is of began at this instant (`reset − length`), after the
    /// baseline.
    NewWindow(i64),
    /// A limit reset was used at this instant, after the baseline: the figure counts from there.
    ResetUsed(i64),
}

/// How far one window moved over a span ([`since_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Since {
    /// The span it was read over, from the baseline to now: the span asked where the baseline
    /// was read at its start (within a reading heartbeat), else the baseline's age, longer where
    /// the series holds no figure near the start and shorter where it holds none that old.
    pub span_s: i64,
    /// What it did.
    pub moved: Moved,
}

/// The newest row of `rows` at or before `now` that parts two accounts, where one does: an
/// `another-account` event, or a `fig` row of another account than `keys` ([`Keys::others`]),
/// which the record's keys tell where no event was written (a label given to another account);
/// and the indices after it, by `t`, a tie in the order given (the file's: an `event` row
/// precedes the `fig` row of its own transaction, which so counts as after it).
fn after_parting(rows: &[SeriesRow], keys: Keys<'_>, now: i64) -> (Option<usize>, Vec<usize>) {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|i| (rows[*i].t, *i));
    let parts = |row: &SeriesRow| match &row.said {
        Said::Event(e) => e.e == EventWord::AnotherAccount,
        Said::Fig { ac, key, .. } => keys.others((ac.as_deref(), *key)),
        Said::Beat | Said::Plan(_) | Said::Close(_) | Said::Day(_) => false,
    };
    match order
        .iter()
        .rposition(|i| rows[*i].t <= now && parts(&rows[*i]))
    {
        Some(at) => {
            let after = order.split_off(at + 1);
            (order.last().copied(), after)
        }
        None => (None, order),
    }
}

/// How far `window`'s figure `current` moved over the last `span_s` before `now`, read against
/// `rows`, a series of the account `keys` names: its baseline is the newest `fig` of the window
/// served at or before `now − span_s`, else the oldest one, and the span is the baseline's age
/// unless it was served within [`accounts::READING_HEARTBEAT_S`] of the span's start; it is
/// taken only after the newest row that parts two accounts ([`after_parting`]), so it never
/// compares two accounts. Where the baseline's reset is not `current`'s, a new window began
/// (`current.reset − length`); else, where a limit reset was used after it (`current`'s epoch, a
/// later row's, or a refill drop, [`runs`]), that instant, the newest; else the rise in whole
/// points, never negative. `None` where `current` is `None` (no figure, a void or cut one, or
/// another account's record) or no `fig` row of the window comes after that parting.
#[must_use]
pub fn since_of(
    rows: &[SeriesRow],
    keys: Keys<'_>,
    (window_id, length_s): (&str, i64),
    current: Option<&WindowFig>,
    span_s: i64,
    now: i64,
) -> Option<Since> {
    let current = current?;
    let figs: Vec<&Fig> = after_parting(rows, keys, now)
        .1
        .into_iter()
        .filter_map(|i| rows[i].fig(window_id))
        .filter(|f| f.at <= now)
        .collect();
    let start = now.saturating_sub(span_s);
    let base = match figs.iter().filter(|f| f.at <= start).max_by_key(|f| f.at) {
        Some(base) => *base,
        None => *figs.iter().min_by_key(|f| f.at)?,
    };
    let span_s = if (0..=accounts::READING_HEARTBEAT_S).contains(&start.saturating_sub(base.at)) {
        span_s
    } else {
        now.saturating_sub(base.at)
    };
    let moved = if base.reset != current.reset {
        Moved::NewWindow(current.reset.saturating_sub(length_s))
    } else {
        let now_fig = Fig::of(current);
        let later: Vec<&Fig> = figs
            .iter()
            .copied()
            .filter(|f| f.reset == current.reset && f.at > base.at)
            .chain((now_fig.at > base.at).then_some(&now_fig))
            .collect();
        let epochs = later
            .iter()
            .filter_map(|f| f.epoch)
            .chain(current.epoch_at)
            .filter(|e| *e > base.at);
        let refills = runs(std::iter::once(base).chain(later.iter().copied()))
            .into_iter()
            .skip(1)
            .filter_map(|run| match run.began {
                Began::Epoch(at) => Some(at),
                Began::Refill => run.figs.first().map(|f| f.at),
                Began::Window => None,
            });
        match epochs.chain(refills).filter(|at| *at <= now).max() {
            Some(at) => Moved::ResetUsed(at),
            None => Moved::Rose(current.whole().saturating_sub(base.whole())),
        }
    };
    Some(Since { span_s, moved })
}

/// The indices of the rows of `rows` (a series of the account `keys` names) that [`since_of`]
/// reads over `span_s` before `now`, for either window: the newest row that parts two accounts
/// ([`after_parting`]), and after it the `fig` rows with a figure served after the span's start
/// and each window's newest figure served at or before it. [`since_of`] says the same over these
/// rows, and over any of `rows` that holds them, as over all of them, so a reader holds no more
/// of a series for its `since` ([`report_view`]).
#[must_use]
pub fn since_rows(rows: &[SeriesRow], keys: Keys<'_>, span_s: i64, now: i64) -> BTreeSet<usize> {
    let start = now.saturating_sub(span_s);
    let (parting, after) = after_parting(rows, keys, now);
    let mut out: BTreeSet<usize> = parting.into_iter().collect();
    for w in [accounts::SESSION_LIMIT, accounts::WEEK_LIMIT] {
        let base = after
            .iter()
            .filter_map(|i| rows[*i].fig(w))
            .map(|f| f.at)
            .filter(|at| *at <= start)
            .max();
        out.extend(after.iter().copied().filter(|i| {
            rows[*i]
                .fig(w)
                .is_some_and(|f| f.at > start || Some(f.at) == base)
        }));
    }
    out
}

/// What a report holds of a series ([`read`]) at `now`: the outlook's view ([`outlook_view`])
/// and the rows `since` over `span_s` reads for the account `keys` names ([`since_rows`]), in
/// file order, so the report never holds a whole series for either.
#[must_use]
pub fn report_view(rows: Vec<SeriesRow>, keys: Keys<'_>, span_s: i64, now: i64) -> Vec<SeriesRow> {
    let since = since_rows(&rows, keys, span_s, now);
    rows.into_iter()
        .enumerate()
        .filter(|(i, row)| in_outlook_view(row, now) || since.contains(i))
        .map(|(_, row)| row)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Compaction

/// How long a row of a known kind this build cannot read is kept ([`Class::Unread`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Horizon {
    /// The raw tier's: a `fig` or a `beat`, which no hour's figure can stand for.
    Raw,
    /// [`HOURLY_S`]: a `close`, whose window it cannot tell.
    Hourly,
    /// [`DAILY_S`]: a `day`, an `event` or a `plan`.
    Daily,
}

/// How a row's kind places it in the tiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Fig,
    Beat,
    Plan,
    FiveClose,
    WeekClose,
    Day,
    Event,
    /// A known kind whose fields this build cannot read (the schema is add-only, so no newer
    /// build writes one): kept to its kind's horizon, never as its hour's figure or the newest
    /// plan.
    Unread(Horizon),
    /// A kind this build does not know, a newer build's: kept to the daily horizon.
    Unknown,
}

impl Class {
    fn of(row: Option<&SeriesRow>, k: &str) -> Class {
        match row.map(|r| &r.said) {
            None => match k {
                "fig" | "beat" => Class::Unread(Horizon::Raw),
                "close" => Class::Unread(Horizon::Hourly),
                "day" | "event" | "plan" => Class::Unread(Horizon::Daily),
                _ => Class::Unknown,
            },
            Some(Said::Fig { .. }) => Class::Fig,
            Some(Said::Beat) => Class::Beat,
            Some(Said::Plan(_)) => Class::Plan,
            Some(Said::Close(c)) if c.w == accounts::SESSION_LIMIT => Class::FiveClose,
            Some(Said::Close(_)) => Class::WeekClose,
            Some(Said::Day(_)) => Class::Day,
            Some(Said::Event(_)) => Class::Event,
        }
    }
}

/// One line a compaction read or derived, written back exactly as `text`.
struct Kept {
    text: String,
    t: i64,
    class: Class,
    /// How late it reaches, for the newest of its class ([`marks`]): a close's last figure, a
    /// day's end, else its `t`.
    reach: i64,
    /// Its weekly figure, where it is a `fig` row with one.
    week: Option<Fig>,
}

impl Kept {
    fn of(text: String, t: i64, row: Option<&SeriesRow>, k: &str) -> Kept {
        let reach = match row.map(|r| &r.said) {
            Some(Said::Close(c)) => c.last,
            Some(Said::Day(d)) => t.saturating_add(d.span_s),
            _ => t,
        };
        Kept {
            text,
            t,
            class: Class::of(row, k),
            reach,
            week: row.and_then(|r| r.fig(accounts::WEEK_LIMIT)).cloned(),
        }
    }
}

/// What [`compact_locked`] left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Compacted {
    /// The rows kept.
    pub rows: usize,
    /// The newest kept row's `t`.
    pub newest: Option<i64>,
    /// The bytes the file holds.
    pub bytes: u64,
    /// Whether the file was rewritten (a pass that changes nothing writes nothing).
    pub rewrote: bool,
}

/// Every schema-1 line of the file at `path` in its last [`READ_MAX`] bytes (the most a reader
/// reads of a file), read line by line, and whether the file held any other (a line before those bytes,
/// past [`ROW_MAX`], not UTF-8, not an envelope, blank, or unterminated).
fn read_kept(path: &Path) -> std::io::Result<(Vec<Kept>, Vec<SeriesRow>, bool)> {
    let Some((mut file, meta)) = super::footer::open_regular_with_meta(path) else {
        return match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok((Vec::new(), Vec::new(), false))
            }
            Err(e) => Err(e),
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: not a regular file", path.display()),
            )),
        };
    };
    let skip = meta.len().saturating_sub(READ_MAX);
    file.seek(SeekFrom::Start(skip))?;
    let mut reader = std::io::BufReader::new(file.take(READ_MAX));
    let (mut kept, mut rows, mut dropped) = (Vec::new(), Vec::new(), false);
    if skip > 0 {
        // The line the cut fell in goes with the lines before it.
        reader.skip_until(b'\n')?;
        dropped = true;
    }
    let mut buf = Vec::with_capacity(ROW_MAX + 1);
    loop {
        buf.clear();
        let cap = u64::try_from(ROW_MAX).unwrap_or(u64::MAX).saturating_add(1);
        if (&mut reader).take(cap).read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        } else if buf.len() > ROW_MAX {
            reader.skip_until(b'\n')?;
            dropped = true;
            continue;
        } else {
            dropped = true;
        }
        let Ok(text) = std::str::from_utf8(&buf) else {
            dropped = true;
            continue;
        };
        let Some(o) = aterm_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v.as_object().cloned())
        else {
            dropped = true;
            continue;
        };
        let Some(envelope) = Envelope::of(&o) else {
            dropped = true;
            continue;
        };
        let row = SeriesRow::of(&o);
        kept.push(Kept::of(
            text.to_owned(),
            envelope.t,
            row.as_ref(),
            &envelope.k,
        ));
        rows.extend(row);
    }
    Ok((kept, rows, dropped))
}

/// The lines a bound never gives up ([`give_up`]): the newest `plan` row, and the newest `day`
/// row and close of each window, which say how far derivation reached, so a pass after the bound
/// derives nothing again from what is left.
fn marks(lines: &[Kept]) -> BTreeSet<usize> {
    [Class::Plan, Class::Day, Class::FiveClose, Class::WeekClose]
        .into_iter()
        .filter_map(|class| {
            lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.class == class)
                .max_by_key(|(i, l)| (l.reach, *i))
                .map(|(i, _)| i)
        })
        .collect()
}

/// The `fig` lines that begin a weekly run ([`runs`]) and the ones just before them: what a
/// weekly close derived after the hourly tier thinned its figures still splits at, so its
/// epochs, first and last figures and peak are the raw rows'.
fn weekly_turns(lines: &[Kept]) -> BTreeSet<usize> {
    let at: BTreeMap<*const Fig, usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.class == Class::Fig)
        .filter_map(|(i, l)| l.week.as_ref().map(|f| (std::ptr::from_ref(f), i)))
        .collect();
    let split = runs(
        lines
            .iter()
            .filter(|l| l.class == Class::Fig)
            .filter_map(|l| l.week.as_ref()),
    );
    let mut out = BTreeSet::new();
    for (i, run) in split.iter().enumerate() {
        let before = i
            .checked_sub(1)
            .and_then(|j| split.get(j))
            .and_then(|prev| prev.figs.last());
        for fig in run.figs.first().into_iter().chain(before) {
            out.extend(at.get(&std::ptr::from_ref(*fig)));
        }
    }
    out
}

/// Which of `lines` the tiers keep at `now` with every row kept for `raw_s`; the newest `plan`
/// row (`plan`) is kept whatever its age.
fn tiered(lines: &[Kept], now: i64, raw_s: i64, plan: Option<usize>) -> Vec<bool> {
    let mut last_of_hour: BTreeMap<i64, usize> = BTreeMap::new();
    for (i, line) in lines.iter().enumerate() {
        if line.class == Class::Fig {
            let hour = line.t.div_euclid(3600);
            let newer = last_of_hour
                .get(&hour)
                .is_none_or(|j| line.t >= lines[*j].t);
            if newer {
                last_of_hour.insert(hour, i);
            }
        }
    }
    let mut hourly_figs: BTreeSet<usize> = last_of_hour.into_values().collect();
    hourly_figs.extend(weekly_turns(lines));
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let age = now.saturating_sub(line.t);
            match line.class {
                _ if Some(i) == plan => true,
                Class::Unread(horizon) => {
                    age < match horizon {
                        Horizon::Raw => raw_s,
                        Horizon::Hourly => HOURLY_S,
                        Horizon::Daily => DAILY_S,
                    }
                }
                _ if age < raw_s => true,
                Class::Fig => age < HOURLY_S && hourly_figs.contains(&i),
                Class::Beat => false,
                Class::FiveClose => age < HOURLY_S,
                Class::Day | Class::WeekClose | Class::Event | Class::Plan | Class::Unknown => {
                    age < DAILY_S
                }
            }
        })
        .collect()
}

fn kept_bytes(lines: &[Kept], keep: &[bool]) -> u64 {
    lines
        .iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.text.len() as u64 + 1)
        .sum()
}

/// Drops the oldest kept lines `pick` names, never one of `marks`, until the kept lines fit in
/// `target` bytes.
fn give_up(
    lines: &[Kept],
    (keep, bytes): (&mut [bool], &mut u64),
    (target, marks): (u64, &BTreeSet<usize>),
    pick: impl Fn(&Kept) -> bool,
) {
    if *bytes <= target {
        return;
    }
    let mut picked: Vec<usize> = (0..lines.len())
        .filter(|i| keep[*i] && !marks.contains(i) && pick(&lines[*i]))
        .collect();
    picked.sort_by_key(|i| lines[*i].t);
    for i in picked {
        if *bytes <= target {
            break;
        }
        keep[i] = false;
        *bytes = bytes.saturating_sub(lines[i].text.len() as u64 + 1);
    }
}

/// Compacts the series at `path` at `now`, its local days under `tz_at`: it derives the
/// anchors' `close` rows ([`derive_closes`]), then the ended days' `day` rows after its newest
/// one ([`day_summaries`]), applies the tiers ([`RAW_S`], [`HOURLY_S`], [`DAILY_S`]; the newest
/// `plan` row is kept whatever its age) and then the bound: past [`BOUND_BYTES`] the raw tier
/// shrinks to [`RAW_SHRUNK_S`] and rows go until the file is [`BOUND_ROOM_BYTES`] under the
/// bound: first the oldest older `fig` and `day` rows and rows of known kinds it cannot read,
/// then the oldest five-hour closes, and last the events, weekly closes, older `plan` rows and
/// rows of unknown kinds; only where all of those are gone do the oldest raw rows go. The newest
/// `plan` row, and the newest `day` row and close of each window, never go to the bound
/// ([`marks`]). It reads at most the file's last [`READ_MAX`] bytes, the most a reader reads of a
/// file (one past it no reader reads at all, and this makes readable again); the kept rows are
/// sorted by `t` and written back exactly as they were read, a staged file of mode 0600 flushed
/// and renamed over it; beside the tiers and the bound, only a line before those bytes or not a
/// schema-1 envelope is dropped. The caller holds [`LOCK_FILE`].
/// # Errors
/// The file could not be read, or its replacement written.
pub fn compact_locked(path: &Path, now: i64, tz_at: fn(i64) -> i64) -> std::io::Result<Compacted> {
    let (mut lines, mut rows, mut changed) = read_kept(path)?;
    let derived = derive_closes(&rows, now);
    rows.extend(derived.iter().cloned());
    let newest_day = rows
        .iter()
        .filter_map(|r| match &r.said {
            Said::Day(d) => Some(r.t.saturating_add(d.span_s)),
            _ => None,
        })
        .max();
    let to = midnight(local_day(now.saturating_sub(CLOSE_SLACK_S), tz_at), tz_at);
    let days = day_summaries(&rows, newest_day.unwrap_or(i64::MIN), to, tz_at);
    for row in derived.iter().chain(&days) {
        if let Some(text) = row.line() {
            changed = true;
            let k = match row.said {
                Said::Close(_) => "close",
                _ => "day",
            };
            lines.push(Kept::of(text, row.t, Some(row), k));
        }
    }
    let marks = marks(&lines);
    let plan = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.class == Class::Plan)
        .max_by_key(|(i, l)| (l.t, *i))
        .map(|(i, _)| i);
    let mut keep = tiered(&lines, now, RAW_S, plan);
    let mut bytes = kept_bytes(&lines, &keep);
    if bytes > BOUND_BYTES {
        keep = tiered(&lines, now, RAW_SHRUNK_S, plan);
        bytes = kept_bytes(&lines, &keep);
        let bound = (BOUND_BYTES - BOUND_ROOM_BYTES, &marks);
        let older = |l: &Kept| now.saturating_sub(l.t) >= RAW_SHRUNK_S;
        give_up(&lines, (&mut keep, &mut bytes), bound, |l| {
            older(l) && matches!(l.class, Class::Fig | Class::Day | Class::Unread(_))
        });
        give_up(&lines, (&mut keep, &mut bytes), bound, |l| {
            older(l) && l.class == Class::FiveClose
        });
        give_up(&lines, (&mut keep, &mut bytes), bound, |l| {
            older(l)
                && matches!(
                    l.class,
                    Class::Event | Class::WeekClose | Class::Plan | Class::Unknown
                )
        });
        give_up(&lines, (&mut keep, &mut bytes), bound, |_| true);
    }
    changed |= keep.iter().any(|k| !k);
    let mut out: Vec<&Kept> = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l)
        .collect();
    let sorted = out.windows(2).all(|pair| pair[0].t <= pair[1].t);
    if !sorted {
        changed = true;
        out.sort_by_key(|l| l.t);
    }
    let left = Compacted {
        rows: out.len(),
        newest: out.iter().map(|l| l.t).max(),
        bytes,
        rewrote: changed,
    };
    if changed {
        let mut text = String::with_capacity(usize::try_from(bytes).unwrap_or(0));
        for line in out {
            text.push_str(&line.text);
            text.push('\n');
        }
        super::upgrade_models::write_atomic_mode(path, &text, Some(0o600))?;
    }
    Ok(left)
}

/// The stamp of the last pass over `dir` ([`COMPACTED_FILE`]).
fn compacted_at(dir: &Path) -> Option<i64> {
    super::footer::read_small(&dir.join(COMPACTED_FILE), 32)?
        .trim()
        .parse()
        .ok()
}

/// Whether a pass over every series in `dir` is due at `now`: no stamp, one [`COMPACT_EVERY_S`]
/// old, or one more than that ahead of `now` (a clock set back).
#[must_use]
pub fn compact_due(dir: &Path, now: i64) -> bool {
    compacted_at(dir).is_none_or(|at| {
        now.saturating_sub(at) >= COMPACT_EVERY_S || at.saturating_sub(now) > COMPACT_EVERY_S
    })
}

/// Stamps a pass over `dir` at `now` ([`COMPACTED_FILE`]). The caller holds [`LOCK_FILE`].
fn stamp_locked(dir: &Path, now: i64) -> std::io::Result<()> {
    super::upgrade_models::write_atomic_mode(
        &dir.join(COMPACTED_FILE),
        &now.to_string(),
        Some(0o600),
    )
}

/// Claims the pass over `dir` that is due at `now`: under the series lock it finds the pass
/// still due and writes the stamp before any series is compacted, so an appender that comes
/// while the pass runs finds it claimed and runs none, and where the stamp cannot be written no
/// pass runs (it would run on every append).
fn claim_pass(dir: &Path, now: i64) -> bool {
    let Ok(Some(_lock)) = lock_within(dir, LOCK_WAIT) else {
        return false;
    };
    compact_due(dir, now) && stamp_locked(dir, now).is_ok()
}

/// Compacts every series in the series directory `dir` at `now` ([`compact_locked`]), the pass
/// stamped first ([`COMPACTED_FILE`]), file by file, each under the series lock taken for that
/// file alone (a file whose lock stays held past [`LOCK_WAIT`] waits for the next pass), and
/// removes a series with no row inside [`DAILY_S`]. No series is removed for the number of them:
/// a roster may hold any number of rows.
/// # Errors
/// The directory could not be listed, its lock taken or the stamp written; a series that fails
/// is left for the next pass.
pub fn compact_dir(dir: &Path, now: i64, tz_at: fn(i64) -> i64) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dir).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(());
    }
    {
        let Some(_lock) = lock_within(dir, LOCK_WAIT)? else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("{}: the series lock stayed held", dir.display()),
            ));
        };
        stamp_locked(dir, now)?;
    }
    pass_over(dir, now, tz_at)
}

/// The pass [`compact_dir`] runs, its stamp written.
fn pass_over(dir: &Path, now: i64, tz_at: fn(i64) -> i64) -> std::io::Result<()> {
    for path in series_paths(dir)? {
        let Ok(Some(_lock)) = lock_within(dir, LOCK_WAIT) else {
            continue;
        };
        let Ok(left) = compact_locked(&path, now, tz_at) else {
            continue;
        };
        if left.newest.is_none_or(|t| now.saturating_sub(t) >= DAILY_S) {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The tail

/// What a [`Tail`] last saw of its file: its identity, length and change time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Stamp {
    ino: u64,
    len: u64,
    mtime_ns: i128,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt as _;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
    Some(Stamp {
        ino,
        len: meta.len(),
        mtime_ns,
    })
}

/// An incremental reader of one series file's outlook view ([`outlook_view`]): a `stat` per
/// [`Tail::refresh`], the appended bytes only while the file grows, the whole file (at most
/// [`READ_MAX`]) after a compaction's rename, and the whole file again after a read that failed.
/// It holds the view's rows only, so what it holds stays the newest week's however long the
/// file's history.
#[derive(Debug, Clone, Default)]
pub struct Tail {
    path: PathBuf,
    /// The file as it was when it was last read whole to its end; `None` until a read succeeds.
    stamp: Option<Stamp>,
    offset: u64,
    rows: Vec<SeriesRow>,
}

impl Tail {
    /// A reader of the file at `path`, nothing read yet.
    #[must_use]
    pub fn new(path: PathBuf) -> Tail {
        Tail {
            path,
            ..Tail::default()
        }
    }

    /// The file it reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The view's rows, in file order.
    #[must_use]
    pub fn rows(&self) -> &[SeriesRow] {
        &self.rows
    }

    /// Reads what changed since the last call, and lets go of the rows that left the view by
    /// `now`; `true` where the rows changed. A read that fails leaves the stamp unset, so the
    /// next call reads the file again.
    pub fn refresh(&mut self, now: i64) -> bool {
        let stamp = stamp_of(&self.path);
        let mut changed = false;
        if stamp != self.stamp {
            let grown = match (self.stamp, stamp) {
                (Some(was), Some(is)) => was.ino == is.ino && is.len >= self.offset,
                _ => false,
            };
            if !grown {
                changed = !self.rows.is_empty();
                self.rows.clear();
                self.offset = 0;
            }
            self.stamp = None;
            if let Some(stamp) = stamp
                && let Some(added) = self.read_on(stamp)
            {
                changed |= added;
                self.stamp = Some(stamp);
            }
        }
        let held = self.rows.len();
        self.rows.retain(|row| in_outlook_view(row, now));
        changed || self.rows.len() != held
    }

    /// Reads the file from the offset to `stamp`'s length (at most [`READ_MAX`]), its whole
    /// lines' `fig`, `beat` and `plan` rows held; whether it held any, or `None` where the read
    /// failed.
    fn read_on(&mut self, stamp: Stamp) -> Option<bool> {
        let mut file = super::footer::open_regular(&self.path)?;
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let max_read = READ_MAX.min(stamp.len.saturating_sub(self.offset));
        let cap = usize::try_from(max_read.saturating_add(1)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(cap);
        file.take(max_read).read_to_end(&mut bytes).ok()?;
        let whole = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        let before = self.rows.len();
        self.rows.extend(
            String::from_utf8_lossy(&bytes[..whole])
                .lines()
                .filter_map(SeriesRow::parse)
                .filter(|row| matches!(row.said, Said::Fig { .. } | Said::Beat | Said::Plan(_))),
        );
        self.offset += whole as u64;
        Some(self.rows.len() != before)
    }
}

#[cfg(test)]
#[path = "account_series_tests.rs"]
mod tests;
