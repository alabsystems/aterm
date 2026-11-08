// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts history [<label|key>] [--since <span>] [--rows] [--json]`: one
//! account's days from this Mac's readings series (`docs/DESIGN-account-stats-2026-10-15.md` §5),
//! one line a local day, with that day's ledger rows and every Mac's journal events about it
//! beneath ([`alab_seen::history`]); with no name, every roster row's, in roster order. Only the
//! account's own readings count: its label's series and, by its keys, any other of this Mac's
//! series that holds its rows (a renamed row's), each row judged by the keys it carries
//! ([`judged`]). On a Mac whose roster has one login, the walls its home's transcripts record
//! from before the series began join the home's row's days ([`account_backfill`], marked
//! `(backfill)`). It stores nothing of its own: no compaction runs and no derived or backfilled
//! row is kept. Nothing calls a provider, and neither a ledger row's free text nor a transcript's
//! text is printed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::{Cli, Env, home_profile, no_home_for, put_time, stats_label};
use crate::harness::account_backfill::{self, Cut, Walls};
use crate::harness::account_series::{
    self, Close, Day, DayFive, DayWeek, Keys, Said, SeriesRow, Stored,
};
use crate::harness::account_stats::{self, day_of, hm, percent, span as span_word};
use crate::harness::accounts::{self, SESSION_LIMIT, WEEK_LIMIT, WEEK_WINDOW_S};
use crate::harness::alab_seen;
use crate::harness::profile::AccountKey;

const DOT: &str = " \u{b7} ";
const DAY_S: i64 = 86_400;

/// The account a block is about: its row's label here, its lab key and its account key, as many
/// as are known, and the name the block goes by.
#[derive(Debug, Clone)]
struct Who {
    label: Option<String>,
    pk: Option<String>,
    key: Option<AccountKey>,
    name: String,
}

impl Who {
    fn asked(&self) -> alab_seen::Asked {
        alab_seen::Asked {
            label: self.label.clone(),
            pk: self.pk.clone(),
        }
    }

    /// The keys that tell this account's series rows from another's.
    fn keys(&self) -> Keys<'_> {
        Keys {
            ac: self.pk.as_deref(),
            key: self.key,
        }
    }

    /// The texts a series row of this account carries (its lab key, its account key), which a
    /// file that may hold one contains ([`by_key`]).
    fn needles(&self) -> Vec<String> {
        self.pk
            .iter()
            .cloned()
            .chain(self.key.map(|key| key.to_string()))
            .collect()
    }
}

/// The account key the state holds for the account a label and a lab key name: the label's
/// record's, where that record is not keyed as another account (its lab key is `pk`'s, or one of
/// them is unknown), else the record's or the forgotten record's that carries the lab key.
fn key_in(state: &accounts::State, label: Option<&str>, pk: Option<&str>) -> Option<AccountKey> {
    label
        .and_then(|label| state.accounts.get(label))
        .filter(|a| pk.is_none() || a.pk.is_none() || a.pk.as_deref() == pk)
        .and_then(|a| a.key)
        .or_else(|| {
            let pk = pk?;
            state
                .accounts
                .values()
                .find(|a| a.pk.as_deref() == Some(pk))
                .or_else(|| state.forgotten.get(pk))
                .and_then(|a| a.key)
        })
}

/// Which account `name` is: what [`alab_seen::resolve`] finds (a row label, a lab key, a lab or
/// legacy label), else an account key (`c-<12 hex>`) matched against the state's records and
/// the forgotten ones; a key nothing here holds is still asked for, by key alone. A key's label
/// is a roster row's only where that row still names the account, as [`alab_seen::resolve`]
/// keeps an explicit lab key's: the row whose lab key is the record's, or, for a record keyed by
/// no lab key, the record's own row where it names none either.
fn resolve(
    cli: Cli,
    name: &str,
    records: &accounts::AccountRecords,
    lab: Option<&crate::harness::alab_roster::Lab>,
) -> Option<Who> {
    let state = &records.state;
    if let Some(asked) = alab_seen::resolve(cli, name, &records.roster, lab) {
        let key = key_in(state, asked.label.as_deref(), asked.pk.as_deref());
        return Some(Who {
            label: asked.label,
            pk: asked.pk,
            key,
            name: name.to_owned(),
        });
    }
    let key = AccountKey::parse(name)?;
    let held = state
        .accounts
        .iter()
        .find(|(_, a)| a.key == Some(key))
        .map(|(label, a)| (Some(label.as_str()), a.pk.clone()))
        .or_else(|| {
            state
                .forgotten
                .iter()
                .find(|(_, a)| a.key == Some(key))
                .map(|(pk, _)| (None, Some(pk.clone())))
        });
    let (label, pk) = held.unwrap_or((None, None));
    let label = match pk.as_deref() {
        Some(pk) => records
            .roster
            .rows
            .iter()
            .find(|row| alab_seen::row_key(cli, row).as_deref() == Some(pk))
            .map(|row| row.label.clone()),
        None => label
            .filter(|label| {
                records
                    .roster
                    .row(label)
                    .is_some_and(|row| alab_seen::row_key(cli, row).is_none())
            })
            .map(str::to_owned),
    };
    Some(Who {
        name: label.clone().unwrap_or_else(|| name.to_owned()),
        label,
        pk,
        key: Some(key),
    })
}

/// The span a history covers: `[from, to]`, `s` seconds.
#[derive(Debug, Clone, Copy)]
struct Span {
    from: i64,
    to: i64,
    s: i64,
}

impl Span {
    const fn holds(self, t: i64) -> bool {
        t >= self.from && t <= self.to
    }
}

/// Whose a series row is ([`judged`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Whose {
    /// It carries a key of the account and none of another.
    Mine,
    /// It carries another account's key.
    Theirs,
    /// Its keys say neither: it carries none, or none of a kind the account's are known in.
    Unknown,
}

/// The lines of one series file that are the account `keys` names, in time order (a tie in file
/// order, so an event precedes the reading of its transaction): a `fig`, `close` or `day` row by
/// the keys it carries ([`Keys::names`], [`Keys::others`]); any other line (a beat, a plan, an
/// event, a kind this build does not read) as the last `fig` row before it, the reading it
/// follows, so another account's sign-in is in the history of the account that held the home. A
/// line whose keys say neither counts only in the label's own file (`own`): where a record held
/// no key, its label's rows are its.
fn judged(mut lines: Vec<Stored>, keys: Keys<'_>, own: bool) -> Vec<Stored> {
    lines.sort_by_key(|line| line.t);
    let mut holder = Whose::Unknown;
    let mut out = Vec::new();
    for line in lines {
        let whose = match line.row.as_ref().and_then(|row| Some((row, row.keys()?))) {
            Some((row, carried)) => {
                let whose = if keys.others(carried) {
                    Whose::Theirs
                } else if keys.names(carried) {
                    Whose::Mine
                } else {
                    Whose::Unknown
                };
                if matches!(row.said, Said::Fig { .. }) {
                    holder = whose;
                }
                whose
            }
            None => holder,
        };
        if whose == Whose::Mine || (own && whose == Whose::Unknown) {
            out.push(line);
        }
    }
    out
}

/// For each of `whos` (an account and its label's own series file, where it has one), the other
/// series files of `machine`'s in the series directory `dir` whose text names one of its keys
/// ([`Who::needles`]): where a reading of it may be, which [`judged`] then tells row by row.
/// Another machine's file is never one: a history is of this Mac's readings. Each file is read
/// once, whatever the number of accounts.
fn by_key(dir: &Path, machine: &str, whos: &[(Who, Option<PathBuf>)]) -> Vec<Vec<PathBuf>> {
    let mut out = vec![Vec::new(); whos.len()];
    let needles: Vec<Vec<String>> = whos.iter().map(|(who, _)| who.needles()).collect();
    if needles.iter().all(Vec::is_empty) {
        return out;
    }
    let paths = account_series::series_paths(dir).unwrap_or_default();
    for path in paths
        .iter()
        .filter(|path| account_series::is_of_machine(path, machine))
    {
        let Ok(Some(text)) = accounts::read_bounded(path, account_series::READ_MAX) else {
            continue;
        };
        for (found, ((_, own), needles)) in out.iter_mut().zip(whos.iter().zip(&needles)) {
            if own.as_ref() != Some(path) && needles.iter().any(|n| text.contains(n.as_str())) {
                found.push(path.clone());
            }
        }
    }
    out
}

/// What this Mac's series hold of one account.
#[derive(Debug, Default)]
struct Series {
    /// Each file's rows of the account, by `t`.
    files: Vec<Vec<SeriesRow>>,
    /// The closes the backfill booked to it inside the span ([`account_backfill::book`]), by
    /// `t`: in memory only.
    backfill: Vec<SeriesRow>,
    /// Their stored lines inside the span, by `t`, where `--rows` asked for them.
    stored: Vec<(i64, String)>,
    /// The oldest of their rows' `t`.
    oldest: Option<i64>,
}

/// The account `who`'s rows of `files` (each with whether it is the label's own), read once
/// each ([`account_series::stored_rows`]) and judged ([`judged`]), and with `stored`, its lines
/// inside `span` as stored.
fn gather(files: &[(PathBuf, bool)], who: &Who, span: Span, stored: bool) -> Series {
    let keys = who.keys();
    let mut series = Series::default();
    for (path, own) in files {
        let kept = judged(account_series::stored_rows(path), keys, *own);
        if kept.is_empty() {
            continue;
        }
        let mut rows = Vec::with_capacity(kept.len());
        for line in kept {
            series.oldest = Some(series.oldest.map_or(line.t, |t| t.min(line.t)));
            if stored && span.holds(line.t) {
                series.stored.push((line.t, line.line));
            }
            rows.extend(line.row);
        }
        series.files.push(rows);
    }
    series.stored.sort_by_key(|(t, _)| *t);
    series
}

/// One file's closes: those it stores and those its compaction would derive
/// ([`account_series::derive_closes`]), one per window anchor.
fn file_closes(rows: &[SeriesRow], now: i64) -> Vec<SeriesRow> {
    rows.iter()
        .filter(|row| matches!(row.said, Said::Close(_)))
        .cloned()
        .chain(account_series::derive_closes(rows, now))
        .collect()
}

/// `b`'s close of an anchor folded into `a`'s: one window whose figures two files hold (a row
/// renamed inside it), from its first figure to its last, its peak, its readings and its resets
/// used, ended at the earlier end either says.
fn fold_close(a: &mut SeriesRow, b: SeriesRow) {
    let ended = a.t.min(b.t);
    let (Said::Close(into), Said::Close(other)) = (&mut a.said, b.said) else {
        return;
    };
    if other.last > into.last {
        into.last = other.last;
        into.end = other.end;
    }
    into.first = into.first.min(other.first);
    into.peak = into.peak.max(other.peak);
    into.walled = into.peak >= 1000;
    into.n = into.n.saturating_add(other.n);
    into.early |= other.early;
    let epochs: BTreeSet<i64> = into.epochs.iter().copied().chain(other.epochs).collect();
    let epochs: Vec<i64> = epochs.into_iter().collect();
    into.epochs = epochs[epochs.len().saturating_sub(account_series::EPOCHS_MAX)..].to_vec();
    into.ac = into.ac.take().or(other.ac);
    into.key = into.key.or(other.key);
    let last = into.last;
    a.t = ended.max(last);
}

/// The account's closes across its files, one per window anchor ([`fold_close`]), by `t`.
fn merged_closes(closes: impl IntoIterator<Item = SeriesRow>) -> Vec<SeriesRow> {
    let mut by: BTreeMap<(&'static str, i64), SeriesRow> = BTreeMap::new();
    for row in closes {
        let Said::Close(c) = &row.said else {
            continue;
        };
        let at = (c.w, c.reset);
        match by.remove(&at) {
            Some(mut held) => {
                fold_close(&mut held, row);
                by.insert(at, held);
            }
            None => {
                by.insert(at, row);
            }
        }
    }
    let mut out: Vec<SeriesRow> = by.into_values().collect();
    out.sort_by_key(|row| row.t);
    out
}

/// One file's days inside `span`: its stored `day` rows (`true`), each made from every reading
/// of its day in that file, and the summaries ([`account_series::day_summaries`], `false`) of its
/// readings and `closes` that no stored row covers, by instant, so a day stored under another
/// zone is not counted again. A beat counts as the reading it repeats.
fn file_days(
    rows: &[SeriesRow],
    closes: &[SeriesRow],
    span: Span,
    tz_at: fn(i64) -> i64,
) -> Vec<(SeriesRow, bool)> {
    let stored: Vec<&SeriesRow> = rows
        .iter()
        .filter(|row| matches!(row.said, Said::Day(_)))
        .collect();
    let covers: BTreeMap<i64, i64> = stored
        .iter()
        .filter_map(|row| match &row.said {
            Said::Day(d) => Some((row.t, row.t.saturating_add(d.span_s))),
            _ => None,
        })
        .collect();
    let covered = |t: i64| {
        covers
            .range(..=t)
            .next_back()
            .is_some_and(|(_, end)| t < *end)
    };
    let mut last_fig: Option<&Said> = None;
    let mut loose: Vec<SeriesRow> = Vec::new();
    for row in rows {
        let said = match &row.said {
            Said::Fig { .. } => {
                last_fig = Some(&row.said);
                &row.said
            }
            Said::Beat => last_fig.unwrap_or(&row.said),
            Said::Plan(_) | Said::Close(_) | Said::Day(_) | Said::Event(_) => continue,
        };
        if !covered(row.t) {
            loose.push(SeriesRow {
                t: row.t,
                by: row.by.clone(),
                said: said.clone(),
            });
        }
    }
    loose.extend(closes.iter().filter(|c| !covered(c.t)).cloned());
    let summaries =
        account_series::day_summaries(&loose, span.from, span.to.saturating_add(1), tz_at);
    stored
        .into_iter()
        .filter(|row| {
            matches!(&row.said, Said::Day(d)
                if row.t <= span.to && row.t.saturating_add(d.span_s) > span.from)
        })
        .map(|row| (row.clone(), true))
        .chain(summaries.into_iter().map(|row| (row, false)))
        .collect()
}

/// `d` added to `into`, the same local day from another file or another part of one: readings
/// and windows summed, the higher peak, the earlier week's first figure and the later one's last
/// (by its reset, then by its figure: in one week a figure only rises but where a limit reset
/// was used).
fn add_day(into: &mut Option<Day>, d: &Day) {
    let Some(a) = into.as_mut() else {
        *into = Some(d.clone());
        return;
    };
    a.n = a.n.saturating_add(d.n);
    a.five_hour = match (a.five_hour, d.five_hour) {
        (Some(x), Some(y)) => Some(DayFive {
            closed: x.closed.saturating_add(y.closed),
            peak: x.peak.max(y.peak),
            walled: x.walled.saturating_add(y.walled),
        }),
        (x, y) => x.or(y),
    };
    a.seven_day = match (a.seven_day, d.seven_day) {
        (Some(x), Some(y)) => {
            let first = if (y.reset, y.first) < (x.reset, x.first) {
                y.first
            } else {
                x.first
            };
            let last = if (y.reset, y.last) > (x.reset, x.last) {
                y
            } else {
                x
            };
            Some(DayWeek {
                first,
                last: last.last,
                reset: last.reset,
                closed: x.closed || y.closed,
            })
        }
        (x, y) => x.or(y),
    };
}

/// Something that happened on a day: a series `event` row's word, or `reset-used`.
#[derive(Debug, Clone)]
struct Happened {
    t: i64,
    e: String,
    w: Option<String>,
}

/// The word a limit reset used is said by.
const RESET_USED: &str = "reset-used";

/// One local day of a history.
#[derive(Debug)]
struct DayOut {
    /// Its local midnight.
    t: i64,
    /// The zone's offset then.
    tz_s: i64,
    /// Its length.
    span_s: i64,
    /// Whether a stored `day` row gave `t`, `tz_s` and `span_s` (the zone it was stored under).
    stored: bool,
    /// What the series says of it.
    sum: Option<Day>,
    /// The five-hour window still open, its last figure, on the day of the row that read it.
    open_x10: Option<u32>,
    /// What happened, by when.
    events: Vec<Happened>,
    /// Its ledger and journal lines, oldest first.
    lines: Vec<String>,
    /// The windows whose part of the day counts a close the backfill booked.
    backfilled: BTreeSet<&'static str>,
}

impl DayOut {
    /// When a new week began this day: its week's reset less a week, inside the day.
    fn new_week(&self) -> Option<i64> {
        let week = self.sum.as_ref()?.seven_day?;
        let began = week.reset.saturating_sub(WEEK_WINDOW_S);
        (began >= self.t && began < self.t.saturating_add(self.span_s)).then_some(began)
    }
}

/// Local day `day`'s entry in `days`, made empty under `tz_at` where it holds none yet.
fn day_entry(days: &mut BTreeMap<i64, DayOut>, day: i64, tz_at: fn(i64) -> i64) -> &mut DayOut {
    days.entry(day).or_insert_with(|| {
        let t = account_series::midnight(day, tz_at);
        DayOut {
            t,
            tz_s: tz_at(t),
            span_s: account_series::midnight(day.saturating_add(1), tz_at).saturating_sub(t),
            stored: false,
            sum: None,
            open_x10: None,
            events: Vec::new(),
            lines: Vec::new(),
            backfilled: BTreeSet::new(),
        }
    })
}

/// The days of `span` that hold anything, oldest first, under `tz_at`. Each file gives its days
/// ([`file_days`]: its stored `day` rows and, over what they do not cover, the summaries of its
/// readings and closes), and so do the backfill's closes, as one more file of closes alone, each
/// part of a day they count in marked ([`DayOut::backfilled`]); these are added up by local date
/// ([`add_day`]); on a day two files both hold (a row renamed that day), each five-hour window
/// is counted once, from the closes of the account's files folded by anchor
/// ([`merged_closes`]), wherever the series still holds a close that ended that day. Over the
/// account's rows of every file with those closes: the five-hour window still open, on the day
/// of the row that read it, the series' events and each limit reset used
/// ([`account_series::resets_used`]). And `lines`, the ledger and journal lines, each on its
/// day.
fn days_of(
    series: &Series,
    lines: &[(i64, String)],
    span: Span,
    tz_at: fn(i64) -> i64,
    now: i64,
) -> Vec<DayOut> {
    let mut days: BTreeMap<i64, DayOut> = BTreeMap::new();
    let local = |t: i64| account_series::local_day(t, tz_at);
    // The account's files, and last the backfill's closes.
    let files: Vec<&Vec<SeriesRow>> = series
        .files
        .iter()
        .chain(std::iter::once(&series.backfill))
        .collect();
    let backfill_at = series.files.len();
    let own_closes: Vec<Vec<SeriesRow>> = files.iter().map(|rows| file_closes(rows, now)).collect();
    let closes = merged_closes(own_closes.iter().flatten().cloned());
    let mut held_by: BTreeMap<i64, BTreeSet<usize>> = BTreeMap::new();
    for (f, (rows, closes)) in files.iter().zip(&own_closes).enumerate() {
        for (row, stored) in file_days(rows, closes, span, tz_at) {
            let Said::Day(d) = &row.said else {
                continue;
            };
            let date = row.t.saturating_add(d.tz_s).div_euclid(DAY_S);
            let day = day_entry(&mut days, date, tz_at);
            if stored && !day.stored {
                (day.t, day.tz_s, day.span_s, day.stored) = (row.t, d.tz_s, d.span_s, true);
            }
            if f == backfill_at {
                if d.five_hour.is_some() {
                    day.backfilled.insert(SESSION_LIMIT);
                }
                if d.seven_day.is_some() {
                    day.backfilled.insert(WEEK_LIMIT);
                }
            }
            add_day(&mut day.sum, d);
            held_by.entry(date).or_default().insert(f);
        }
    }
    let fives: Vec<(i64, &Close)> = closes
        .iter()
        .filter_map(|row| match &row.said {
            Said::Close(c) if c.w == SESSION_LIMIT => Some((row.t, c)),
            _ => None,
        })
        .collect();
    for (date, files) in &held_by {
        let ended: Vec<&Close> = fives
            .iter()
            .filter(|(t, _)| local(*t) == *date)
            .map(|(_, c)| *c)
            .collect();
        if files.len() < 2 || ended.is_empty() {
            continue;
        }
        if let Some(five) = days
            .get_mut(date)
            .and_then(|day| day.sum.as_mut())
            .and_then(|sum| sum.five_hour.as_mut())
        {
            five.closed = u32::try_from(ended.len()).unwrap_or(u32::MAX);
            five.walled = u32::try_from(ended.iter().filter(|c| c.walled).count()).unwrap_or(0);
        }
    }
    // Every file's rows of the account as one series, its closes folded by anchor.
    let mut rows: Vec<SeriesRow> = files
        .iter()
        .copied()
        .flatten()
        .filter(|row| !matches!(row.said, Said::Close(_)))
        .cloned()
        .chain(closes.iter().cloned())
        .collect();
    rows.sort_by_key(|row| row.t);
    let closed: BTreeSet<i64> = fives.iter().map(|(_, c)| c.reset).collect();
    if let Some((t, open)) = rows
        .iter()
        .filter_map(|row| Some((row.t, row.fig(SESSION_LIMIT)?)))
        .filter(|(t, f)| !closed.contains(&f.reset) && f.reset > now && span.holds(*t))
        .max_by_key(|(t, _)| *t)
    {
        day_entry(&mut days, local(t), tz_at).open_x10 = Some(open.pct_x10);
    }
    for row in &rows {
        if let Said::Event(e) = &row.said {
            let at = e.at.unwrap_or(row.t);
            if span.holds(at) {
                day_entry(&mut days, local(at), tz_at)
                    .events
                    .push(Happened {
                        t: at,
                        e: e.e.as_str().to_owned(),
                        w: e.w.clone(),
                    });
            }
        }
    }
    for w in [SESSION_LIMIT, WEEK_LIMIT] {
        for at in account_series::resets_used(&rows, w, now) {
            if span.holds(at) {
                day_entry(&mut days, local(at), tz_at)
                    .events
                    .push(Happened {
                        t: at,
                        e: RESET_USED.to_owned(),
                        w: Some(w.to_owned()),
                    });
            }
        }
    }
    for (t, line) in lines {
        if span.holds(*t) {
            day_entry(&mut days, local(*t), tz_at)
                .lines
                .push(line.clone());
        }
    }
    let mut out: Vec<DayOut> = days.into_values().collect();
    for day in &mut out {
        day.events.sort_by_key(|e| e.t);
    }
    out
}

/// A whole percent.
fn pct(x10: u32) -> String {
    percent(x10, false)
}

/// An instant on its own local clock: `14:10`.
fn clock(t: i64, tz_at: fn(i64) -> i64) -> String {
    hm(t, tz_at(t))
}

/// What happened, in words: `another account signed in to the home 14:02`, `reset used 14:10`
/// (the five-hour window's), `week reset used 09:00`; a word this build does not know is printed
/// as the word.
fn happened_words(e: &Happened, tz_at: fn(i64) -> i64) -> String {
    let at = clock(e.t, tz_at);
    match (e.e.as_str(), e.w.as_deref()) {
        ("another-account", _) => format!("another account signed in to the home {at}"),
        (RESET_USED, Some(WEEK_LIMIT)) => format!("week reset used {at}"),
        (RESET_USED, _) => format!("reset used {at}"),
        (word, Some(w)) => format!("{word} ({w}) {at}"),
        (word, None) => format!("{word} {at}"),
    }
}

/// One day's line: `Sat Nov 7  week 58% → 71% (resets Sun Nov 8 20:50) · 5h 3 windows, peak
/// 100%, 1 full · reset used 14:10`; the five-hour peak is the day's highest five-hour figure. A
/// part that counts a close the backfill booked ends `(backfill)`.
fn day_line(day: &DayOut, tz_at: fn(i64) -> i64) -> String {
    let marked = |w: &str| {
        if day.backfilled.contains(w) {
            " (backfill)"
        } else {
            ""
        }
    };
    let at = |t: i64| format!("{} {}", day_of(t, tz_at(t)), clock(t, tz_at));
    let mut parts = Vec::new();
    if let Some(week) = day.sum.as_ref().and_then(|s| s.seven_day) {
        let figures = if week.first == week.last {
            pct(week.last)
        } else {
            format!("{} \u{2192} {}", pct(week.first), pct(week.last))
        };
        let mut words = format!("resets {}", at(week.reset));
        if let Some(began) = day.new_week() {
            words = format!("new week {}, {words}", clock(began, tz_at));
        }
        parts.push(format!("week {figures} ({words}){}", marked(WEEK_LIMIT)));
    }
    if let Some(five) = day.sum.as_ref().and_then(|s| s.five_hour) {
        let mut bits = Vec::new();
        match five.closed {
            0 => {}
            1 => bits.push("1 window".to_owned()),
            n => bits.push(format!("{n} windows")),
        }
        bits.push(format!("peak {}", pct(five.peak)));
        if five.walled > 0 {
            bits.push(format!("{} full", five.walled));
        }
        if let Some(open) = day.open_x10 {
            bits.push(format!("open {}", pct(open)));
        }
        parts.push(format!("5h {}{}", bits.join(", "), marked(SESSION_LIMIT)));
    }
    parts.extend(day.events.iter().map(|e| happened_words(e, tz_at)));
    let date = day_of(day.t, day.tz_s);
    if parts.is_empty() {
        format!("  {date}")
    } else {
        format!("  {date}  {}", parts.join(DOT))
    }
}

/// A local day as `2026-11-07`.
fn date_of(t: i64, tz_s: i64) -> String {
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(t.saturating_add(tz_s).div_euclid(DAY_S));
    format!("{y:04}-{m:02}-{d:02}")
}

fn day_json(day: &DayOut) -> Value {
    let mut o = Map::new();
    o.insert("date".to_owned(), Value::from(date_of(day.t, day.tz_s)));
    put_time(&mut o, "t", Some(day.t));
    o.insert("tz_s".to_owned(), Value::from(day.tz_s));
    let sum = day.sum.as_ref();
    o.insert(
        "five_hour".to_owned(),
        sum.and_then(|s| s.five_hour).map_or(Value::Null, |five| {
            let mut f = Map::new();
            f.insert("windows".to_owned(), Value::from(five.closed));
            f.insert("peak_pct_x10".to_owned(), Value::from(five.peak));
            f.insert("full".to_owned(), Value::from(five.walled));
            f.insert("open_pct_x10".to_owned(), Value::from(day.open_x10));
            f.insert(
                "backfill".to_owned(),
                Value::from(day.backfilled.contains(SESSION_LIMIT)),
            );
            Value::Object(f)
        }),
    );
    o.insert(
        "seven_day".to_owned(),
        sum.and_then(|s| s.seven_day).map_or(Value::Null, |week| {
            let mut w = Map::new();
            w.insert("first_pct_x10".to_owned(), Value::from(week.first));
            w.insert("last_pct_x10".to_owned(), Value::from(week.last));
            put_time(&mut w, "resets_at", Some(week.reset));
            put_time(&mut w, "new_window", day.new_week());
            w.insert(
                "backfill".to_owned(),
                Value::from(day.backfilled.contains(WEEK_LIMIT)),
            );
            Value::Object(w)
        }),
    );
    o.insert(
        "events".to_owned(),
        Value::Array(
            day.events
                .iter()
                .map(|e| {
                    let mut v = Map::new();
                    put_time(&mut v, "t", Some(e.t));
                    v.insert("e".to_owned(), Value::from(e.e.clone()));
                    v.insert("w".to_owned(), Value::from(e.w.clone()));
                    Value::Object(v)
                })
                .collect(),
        ),
    );
    Value::Object(o)
}

/// The home the environment names, as the backfill reads it ([`account_backfill`]).
struct Home {
    /// The label `accounts stats` names it by ([`stats_label`]).
    label: String,
    /// What its transcripts hold and the newest instant another account was seen on it
    /// ([`account_stats::another_seen`]), or why nothing is booked to it: `not-the-home` (no
    /// block of its label is made, so nothing is read), `several-logins` (the roster lists
    /// several, so its walls may be any of theirs), `no-home` (no Claude Code directory is known)
    /// or `another-account` (the label's record is another account's than the one the home's
    /// profile names).
    read: Result<(Walls, Option<i64>), &'static str>,
}

impl Home {
    /// The home `env` names, of `records`, read for a history over `span` where `wanted` says a
    /// block of its label is made: its transcripts last written since the span's start less a
    /// week (no close of a wall before then ends inside the span) and since another account was
    /// last seen on it, within [`account_backfill::READ_BYTES`] and [`super::STATS_FOLD_S`].
    fn of(
        env: &Env,
        records: &accounts::AccountRecords,
        span: Span,
        wanted: impl FnOnce(&str) -> bool,
    ) -> Home {
        let profile = home_profile(env);
        let key = profile.as_ref().ok().and_then(|p| p.key);
        let label = stats_label(env, key, &records.roster);
        let read = if !wanted(&label) {
            Err("not-the-home")
        } else if records.roster.several() {
            Err("several-logins")
        } else if let Some(dir) = env.claude_dir() {
            let record = records.state.accounts.get(&label);
            if record.is_some_and(|a| a.of_another(key)) {
                Err("another-account")
            } else {
                let seen = account_stats::another_seen(
                    env.now,
                    records.roster.several(),
                    record,
                    profile.as_ref().ok(),
                );
                let since = span
                    .from
                    .saturating_sub(WEEK_WINDOW_S)
                    .max(seen.unwrap_or(i64::MIN));
                let start = std::time::Instant::now();
                let limits = crate::harness::spend::Limits {
                    bytes: account_backfill::READ_BYTES,
                    until: start.checked_add(std::time::Duration::from_secs(super::STATS_FOLD_S)),
                };
                let walls = account_backfill::read(&dir.join("projects"), since, &limits, env.now);
                Ok((walls, seen))
            }
        } else {
            Err("no-home")
        };
        Home { label, read }
    }
}

/// What the backfill did for one account's block: how many closes it booked inside the span,
/// whether the transcripts were read whole (else what cut the read), or why it booked nothing
/// (the home's [`Home::read`], or `not-the-home` for an account other than the home's).
#[derive(Debug, Clone, Copy)]
struct Booked {
    booked: usize,
    cut: Option<Cut>,
    why: Option<&'static str>,
}

/// One account's history: who it is, its ledger and journal lines (all of them, with their
/// instants), and, for Claude Code's pool, its series, its days and what the backfill did.
struct Block {
    who: Who,
    lines: Vec<(i64, String)>,
    series: Option<(Series, Vec<DayOut>, Booked)>,
}

impl Block {
    /// Whether the block holds nothing to go by: no row, no lab key and no series row.
    fn is_unknown(&self) -> bool {
        self.who.label.is_none()
            && self.who.pk.is_none()
            && self
                .series
                .as_ref()
                .is_none_or(|(s, _, _)| s.files.is_empty() && s.backfill.is_empty())
    }

    /// The account's object in `--json`: the lab model's `{label, ac, journal, lines}` and, with
    /// a series, `span`, `series`, `days`, `backfill` (`{booked, complete, cut, why, closes}`,
    /// each close as a series row would say it, `src: "backfill"`) and (with `--rows`) `rows`, the
    /// stored rows alone; without one, `series: false`.
    fn json(&self, span: Span, (machine, journal, rows): (&str, bool, bool)) -> Map {
        let mut o = Map::new();
        o.insert("label".to_owned(), Value::from(self.who.label.clone()));
        o.insert("ac".to_owned(), Value::from(self.who.pk.clone()));
        o.insert("journal".to_owned(), Value::from(journal));
        o.insert(
            "lines".to_owned(),
            Value::Array(
                self.lines
                    .iter()
                    .map(|(_, line)| Value::from(line.clone()))
                    .collect(),
            ),
        );
        let Some((series, days, booked)) = &self.series else {
            o.insert("series".to_owned(), Value::from(false));
            return o;
        };
        let mut s = Map::new();
        put_time(&mut s, "from", Some(span.from));
        put_time(&mut s, "to", Some(span.to));
        o.insert("span".to_owned(), Value::Object(s));
        let mut m = Map::new();
        m.insert("machine".to_owned(), Value::from(machine));
        put_time(&mut m, "oldest", series.oldest);
        o.insert("series".to_owned(), Value::Object(m));
        o.insert(
            "days".to_owned(),
            Value::Array(days.iter().map(day_json).collect()),
        );
        let mut b = Map::new();
        b.insert(
            "booked".to_owned(),
            Value::from(u64::try_from(booked.booked).unwrap_or(u64::MAX)),
        );
        b.insert("complete".to_owned(), Value::from(booked.cut.is_none()));
        b.insert("cut".to_owned(), Value::from(booked.cut.map(Cut::as_str)));
        b.insert("why".to_owned(), Value::from(booked.why));
        b.insert(
            "closes".to_owned(),
            Value::Array(
                series
                    .backfill
                    .iter()
                    .filter_map(SeriesRow::line)
                    .filter_map(|line| aterm_json::from_str::<Value>(&line).ok())
                    .collect(),
            ),
        );
        o.insert("backfill".to_owned(), Value::Object(b));
        if rows {
            o.insert(
                "rows".to_owned(),
                Value::Array(
                    series
                        .stored
                        .iter()
                        .map(|(_, line)| Value::from(line.clone()))
                        .collect(),
                ),
            );
        }
        o
    }

    /// The account's block of text: its header (which says `transcripts read in part (<cut>)`
    /// where the backfill's read was cut), then each day's line with that day's ledger and
    /// journal lines indented beneath, or `nothing recorded in 30d`; with `--rows`, the account's
    /// stored rows inside the span after it, exactly as stored.
    fn text(&self, cli: Cli, span: Span, rows: bool, tz_at: fn(i64) -> i64) -> String {
        let mut out = String::new();
        let Some((series, days, booked)) = &self.series else {
            let _ = writeln!(out, "{}{DOT}{}", self.who.name, cli.id());
            if self.lines.is_empty() {
                out.push_str("  nothing recorded\n");
            }
            for (_, line) in &self.lines {
                let _ = writeln!(out, "  {line}");
            }
            return out;
        };
        let at = |t: i64| day_of(t, tz_at(t));
        let _ = write!(
            out,
            "{}{DOT}{}{DOT}this Mac{DOT}{} \u{2013} {} ({})",
            self.who.name,
            cli.id(),
            at(span.from),
            at(span.to),
            span_word(span.s)
        );
        match series.oldest {
            Some(t) => {
                let _ = write!(out, "{DOT}series since {}", at(t));
            }
            None => out.push_str(" \u{b7} no series yet"),
        }
        if let Some(cut) = booked.cut {
            let _ = write!(out, "{DOT}transcripts read in part ({})", cut.as_str());
        }
        out.push('\n');
        if days.is_empty() {
            let _ = writeln!(out, "  nothing recorded in {}", span_word(span.s));
        }
        for day in days {
            let _ = writeln!(out, "{}", day_line(day, tz_at));
            for line in &day.lines {
                let _ = writeln!(out, "    {line}");
            }
        }
        if rows {
            for (_, line) in &series.stored {
                let _ = writeln!(out, "{line}");
            }
        }
        out
    }
}

/// `accounts history` ([`super::AccountsCmd::History`]) of `cli`'s pool on `env`, its days on the
/// local clock `tz_at` gives (the machine's own where a person ran it, [`super::report_clock`]).
/// Each account's block is made and printed in turn, so no more than one account's series is
/// held at once.
pub(super) fn run(
    cli: Cli,
    env: &Env,
    (name, json, since_s, rows): (Option<&str>, bool, i64, bool),
    tz_at: fn(i64) -> i64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm harness accounts history: {why}");
        ExitCode::FAILURE
    };
    if name.is_some_and(|name| name.contains('@')) {
        return refuse(
            err,
            "an account is named by its label or key, never its address",
        );
    }
    let (Some(path), Some(store)) = (env.accounts_roster_for(cli), env.accounts_store_for(cli))
    else {
        return refuse(err, &no_home_for(cli));
    };
    let Ok(records) = store.read_account_records(Some(&path), env.now) else {
        return refuse(err, "the account records could not be read consistently");
    };
    let whos: Vec<Who> = match name {
        Some(name) => {
            let lab = crate::harness::alab_roster::current();
            match resolve(cli, name, &records, lab.as_deref()) {
                Some(who) => vec![who],
                None => return refuse(err, "no row, lab label or account key goes by that name"),
            }
        }
        None => records
            .roster
            .rows
            .iter()
            .map(|row| {
                let pk = alab_seen::row_key(cli, row);
                Who {
                    label: Some(row.label.clone()),
                    key: key_in(&records.state, Some(&row.label), pk.as_deref()),
                    pk,
                    name: row.label.clone(),
                }
            })
            .collect(),
    };
    let span = Span {
        from: env.now.saturating_sub(since_s),
        to: env.now,
        s: since_s,
    };
    let machine = records.state.machine.as_str();
    // The readings series is Claude Code's alone (the usage tap writes it).
    let whos: Vec<(Who, Option<PathBuf>)> = whos
        .into_iter()
        .map(|who| {
            let own = who
                .label
                .as_deref()
                .filter(|_| cli == Cli::Claude)
                .and_then(|label| account_series::path_of(&env.state, label, machine));
            (who, own)
        })
        .collect();
    let elsewhere = if cli == Cli::Claude {
        by_key(&account_series::dir_in(&env.state), machine, &whos)
    } else {
        vec![Vec::new(); whos.len()]
    };
    let ledger = store.tail(accounts::LEDGER_KEEP_ROWS).unwrap_or_default();
    let seen = alab_seen::current(true);
    let flags = (machine, seen.is_some(), rows);
    // The home the environment names, read only where a block of its label is made.
    let home = (cli == Cli::Claude).then(|| {
        Home::of(env, &records, span, |label| {
            whos.iter()
                .any(|(who, _)| who.label.as_deref() == Some(label))
        })
    });
    let block_of = |who: Who, own: Option<PathBuf>, elsewhere: Vec<PathBuf>| {
        let journal = seen
            .as_deref()
            .zip(who.pk.as_deref())
            .and_then(|(seen, pk)| seen.accounts.get(pk));
        let lines = alab_seen::history(&who.asked(), &ledger, journal);
        let series = home.as_ref().map(|home| {
            let files: Vec<(PathBuf, bool)> = own
                .into_iter()
                .map(|path| (path, true))
                .chain(elsewhere.into_iter().map(|path| (path, false)))
                .collect();
            let mut series = gather(&files, &who, span, rows);
            let booked = match &home.read {
                _ if who.label.as_deref() != Some(home.label.as_str()) => Booked {
                    booked: 0,
                    cut: None,
                    why: Some("not-the-home"),
                },
                Err(why) => Booked {
                    booked: 0,
                    cut: None,
                    why: Some(why),
                },
                Ok((walls, another_seen)) => {
                    series.backfill = account_backfill::book(
                        walls,
                        series.files.iter().flatten(),
                        *another_seen,
                        ((who.pk.as_deref(), who.key), machine),
                        env.now,
                    );
                    series.backfill.retain(|close| span.holds(close.t));
                    Booked {
                        booked: series.backfill.len(),
                        cut: walls.cut,
                        why: None,
                    }
                }
            };
            let days = days_of(&series, &lines, span, tz_at, env.now);
            (series, days, booked)
        });
        Block { who, lines, series }
    };
    let ledger_only = || {
        if seen.is_none() {
            "(this Mac's ledger only: no lab journal is read in this process)\n"
        } else {
            ""
        }
    };
    if name.is_some() {
        let Some(((who, own), elsewhere)) = whos.into_iter().zip(elsewhere).next() else {
            return refuse(err, "no row, lab label or account key goes by that name");
        };
        let block = block_of(who, own, elsewhere);
        if block.is_unknown() {
            return refuse(err, "no row, lab label or account key goes by that name");
        }
        if json {
            let mut doc = Map::new();
            doc.insert("kind".to_owned(), Value::from("account-history"));
            doc.insert("schema".to_owned(), Value::from(1u64));
            doc.extend(block.json(span, flags));
            let _ = writeln!(
                out,
                "{}",
                aterm_json::to_string(&Value::Object(doc)).unwrap_or_default()
            );
        } else if cli == Cli::Claude {
            let _ = write!(
                out,
                "{}{}",
                ledger_only(),
                block.text(cli, span, rows, tz_at)
            );
        } else {
            // Another CLI's named history keeps the lab model's lines as they were.
            let _ = write!(out, "{}", ledger_only());
            for (_, line) in &block.lines {
                let _ = writeln!(out, "{line}");
            }
        }
        return ExitCode::SUCCESS;
    }
    if json {
        let accounts: Vec<Value> = whos
            .into_iter()
            .zip(elsewhere)
            .map(|((who, own), elsewhere)| {
                Value::Object(block_of(who, own, elsewhere).json(span, flags))
            })
            .collect();
        let mut doc = Map::new();
        doc.insert("kind".to_owned(), Value::from("account-history"));
        doc.insert("schema".to_owned(), Value::from(1u64));
        doc.insert("accounts".to_owned(), Value::Array(accounts));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(doc)).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let _ = write!(out, "{}", ledger_only());
    for (i, ((who, own), elsewhere)) in whos.into_iter().zip(elsewhere).enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        let block = block_of(who, own, elsewhere);
        let _ = write!(out, "{}", block.text(cli, span, rows, tz_at));
    }
    ExitCode::SUCCESS
}
