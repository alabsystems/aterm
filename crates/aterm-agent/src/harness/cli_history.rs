// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts history [<account>] [--since <span>] [--rows] [--json]`: one
//! account's days from this Mac's readings series (`docs/DESIGN-account-stats-2026-10-15.md` §5),
//! one line a local day, with that day's ledger rows and every Mac's journal events about it
//! beneath ([`alab_seen::history`]); with no account named, every roster row's, in roster order.
//! The account is named as every accounts verb names one ([`account_names::resolve`]: its slug,
//! its key, its provider word and address, or its address), and printed by its names, never by
//! its row's label, the local key the series files and the ledger still hold. Only the
//! account's own readings count: its row's series and, by its keys, any other of this Mac's
//! series that holds its rows (a renamed row's), each row judged by the keys it carries (the
//! fold is [`crate::harness::account_history`]'s, which the Account card shares). On a Mac whose
//! roster has one login, the walls its home's transcripts record of windows the series holds no
//! word of join the home's row's days ([`account_backfill`], marked `(backfill)`). It stores
//! nothing of its own: no compaction runs and no derived or backfilled row is kept. Nothing calls
//! a provider, and neither a ledger row's free text nor a transcript's text is printed.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::{Cli, Env, home_profile, no_home_for, put_time, stats_home};
use crate::harness::account_backfill::{self, Cut, Walls};
use crate::harness::account_history::{
    Happened, HistoryDay, HistoryWho, RESET_USED, Series, Span, by_key, days_of, gather,
};
use crate::harness::account_names::{self, Candidates, Named};
use crate::harness::account_series::{self, SeriesRow};
use crate::harness::account_stats::{self, day_of, hm, span as span_word};
use crate::harness::accounts::{self, Row, SESSION_LIMIT, WEEK_LIMIT, WEEK_WINDOW_S};
use crate::harness::alab_roster::Lab;
use crate::harness::alab_seen;
use crate::harness::profile::AccountKey;
use aterm_file::AccountNames;

const DOT: &str = " \u{b7} ";
const DAY_S: i64 = 86_400;

/// The `account-history` JSON's schema: 2 names each account by its key, slug and name, where 1
/// named it by its roster label.
const HISTORY_SCHEMA: u64 = 2;

/// The account a block is about ([`HistoryWho`]: its row's label here, its lab key and its
/// account key, as many as are known) and its names.
#[derive(Debug, Clone)]
struct Who {
    of: HistoryWho,
    /// Its names; `None` for a roster row with no address, which has no key and no names.
    names: Option<AccountNames>,
}

impl Who {
    /// The account `named` resolved to, its row here where it has one; its account key the one
    /// the state holds for that row or for its lab key ([`key_in`]).
    fn of_named(named: Named<'_>, state: &accounts::State) -> Who {
        let label = named.row.map(|row| row.label.clone());
        let key = key_in(state, label.as_deref(), Some(&named.pk));
        Who {
            of: HistoryWho {
                label,
                pk: Some(named.pk),
                key,
            },
            names: Some(named.names),
        }
    }

    /// `cli`'s roster row `row`, named as [`account_names::names_of_row`] names its account; a
    /// row with no address has no names.
    fn of_row(cli: Cli, row: &Row, state: &accounts::State, lab: Option<&Lab>) -> Who {
        match account_names::names_of_row(cli, row, lab) {
            Some((pk, names)) => Who::of_named(
                Named {
                    pk,
                    row: Some(row),
                    names,
                },
                state,
            ),
            None => Who {
                of: HistoryWho {
                    label: Some(row.label.clone()),
                    pk: None,
                    key: key_in(state, Some(&row.label), None),
                },
                names: None,
            },
        }
    }

    fn asked(&self) -> alab_seen::Asked {
        alab_seen::Asked {
            label: self.of.label.clone(),
            pk: self.of.pk.clone(),
        }
    }

    /// The account as its block's header shows it: its name and slug ([`Named::shown`]); a row
    /// with no address by its label, marked so, since it has no other name.
    fn shown(&self) -> String {
        match (&self.names, &self.of.pk) {
            (Some(names), Some(pk)) => Named {
                pk: pk.clone(),
                row: None,
                names: names.clone(),
            }
            .shown(),
            _ => format!(
                "{} (no address)",
                self.of.label.as_deref().unwrap_or_default()
            ),
        }
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

/// The account keys (`ac-…`) this Mac's records of the pool hold: each record's and each
/// forgotten record's ([`Candidates::held`]), so a key names its account here while this Mac
/// keeps its observations, though no row holds it now and no lab copy names it.
fn held_keys(state: &accounts::State) -> Vec<String> {
    let mut keys: Vec<String> = state
        .accounts
        .values()
        .filter_map(|account| account.pk.clone())
        .chain(state.forgotten.keys().cloned())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// The account `word` names among this Mac's roster rows, the lab's accounts of `cli`'s pool and
/// the account keys this Mac's records hold ([`account_names::resolve`]): its slug, its key
/// (`ac-…`), its provider word and address, or an address that names one account of the pool.
///
/// # Errors
/// The resolver's refusal: `word` names no account of the pool (a roster label names none),
/// several, or one while the roster row labelled with it holds another.
fn resolve(
    cli: Cli,
    word: &str,
    records: &accounts::AccountRecords,
    lab: Option<&Lab>,
) -> Result<Who, String> {
    let held = held_keys(&records.state);
    let candidates = Candidates {
        roster: &records.roster.rows,
        logins: &[],
        lab,
        held: &held,
    };
    account_names::resolve(cli, word, &candidates).map(|named| Who::of_named(named, &records.state))
}

/// What a ledger line says of an account a ledger row holds by a label this Mac's records do not
/// place with the account its row holds now ([`held_then`]): the label may have changed hands.
const HELD_THEN: &str = "(an account a row here held then)";

/// Whether this Mac's records say the roster row labelled `label` held the account `pk` (the one
/// it holds now) at `t`: the state's record of the label carries `pk` as its lab key (a
/// transaction learns it from the row, [`crate::harness::journal::learn`]) and was not keyed to
/// another account after `t` (`keyed_at`), and every ledger row of the label that carried a key,
/// from the newest at or before `t` on, carried `pk`. Where the record is missing or holds no
/// key, nothing says whose the label was, and the answer is no: a label is never named as the
/// account its row holds now unless the records place it there then.
fn held_then(
    label: &str,
    pk: &str,
    t: i64,
    state: &accounts::State,
    ledger: &[accounts::Event],
) -> bool {
    let record = state.accounts.get(label);
    if record.is_none_or(|account| {
        account.pk.as_deref() != Some(pk) || account.keyed_at.is_some_and(|at| at > t)
    }) {
        return false;
    }
    let keyed: Vec<(i64, &str)> = ledger
        .iter()
        .filter(|event| event.account == label)
        .filter_map(|event| Some((event.t, event.pk.as_deref()?)))
        .collect();
    let since = keyed
        .iter()
        .map(|(at, _)| *at)
        .filter(|at| *at <= t)
        .max()
        .unwrap_or(i64::MIN);
    keyed
        .iter()
        .filter(|(at, _)| *at >= since)
        .all(|(_, held)| *held == pk)
}

/// How a ledger line names an account a ledger row holds by its row label, at the row's instant
/// ([`alab_seen::history`]): by the key the row carried (`pk`, for the account the row is about)
/// with the slug the lab gives it; else, where the records say the roster row labelled so held
/// then the account it holds now ([`held_then`]), by that account's key and slug
/// ([`Named::logged`]), and otherwise as [`HELD_THEN`]; a row with no address by its label,
/// marked so, since it has no other name; and a label no row here holds now as `(a row no
/// longer here)`. Never an address. A rotation row's `to=` carries no key, so it is always
/// named this way.
pub(super) fn ledger_name<'a>(
    cli: Cli,
    records: &'a accounts::AccountRecords,
    ledger: &'a [accounts::Event],
    lab: Option<&'a Lab>,
) -> impl Fn(&str, Option<&str>, i64) -> String + 'a {
    move |label, pk, t| {
        if let Some(pk) = pk {
            let names = lab
                .and_then(|lab| lab.names.get(pk))
                .cloned()
                .unwrap_or_default();
            return Named {
                pk: pk.to_owned(),
                row: None,
                names,
            }
            .logged();
        }
        let Some(row) = records.roster.rows.iter().find(|row| row.label == label) else {
            return "(a row no longer here)".to_owned();
        };
        match account_names::names_of_row(cli, row, lab) {
            Some((pk, names)) if held_then(label, &pk, t, &records.state, ledger) => Named {
                pk,
                row: Some(row),
                names,
            }
            .logged(),
            Some(_) => HELD_THEN.to_owned(),
            None => format!("{label} (no address)"),
        }
    }
}

/// What is left of a window at `x10` tenths used, in whole percent: `29% left`
/// ([`crate::harness::left`]).
fn left(x10: u32) -> String {
    crate::harness::left::words(x10)
}

/// An instant on its own local clock: `14:10`.
fn clock(t: i64, tz_at: fn(i64) -> i64) -> String {
    hm(t, tz_at(t))
}

/// What happened, in words: `another account signed in to the home 14:02`, `5h refilled or
/// raised 14:10`, `week refilled or raised 09:00` (the series does not tell a limit reset spent
/// from a refill or a raised allowance, [`RESET_USED`]), or `week reset used 09:00` where this
/// Mac's ledger shows its aterm spent one then ([`Happened::spent`]), each window named as the
/// day line's own part names it; a word this build does not know is printed as the word.
fn happened_words(e: &Happened, tz_at: fn(i64) -> i64) -> String {
    let at = clock(e.t, tz_at);
    match (e.e.as_str(), e.w.as_deref()) {
        ("another-account", _) => format!("another account signed in to the home {at}"),
        (RESET_USED, w) => {
            let window = match w {
                Some(WEEK_LIMIT) => "week ",
                Some(SESSION_LIMIT) => "5h ",
                _ => "",
            };
            let what = if e.spent {
                "reset used"
            } else {
                "refilled or raised"
            };
            format!("{window}{what} {at}")
        }
        (word, Some(w)) => format!("{word} ({w}) {at}"),
        (word, None) => format!("{word} {at}"),
    }
}

/// One day's line: `Sat Nov 7  week 42% → 29% left (resets Sun Nov 8 20:50) · 5h 3 windows,
/// lowest 0% left, 1 used up · 5h refilled or raised 14:10`; the five-hour lowest is what was left at the
/// day's highest five-hour figure. A part that counts a close the backfill booked ends
/// `(backfill)`.
fn day_line(day: &HistoryDay, tz_at: fn(i64) -> i64) -> String {
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
            left(week.last)
        } else {
            format!(
                "{}% \u{2192} {}",
                crate::harness::left::exact(week.first),
                left(week.last)
            )
        };
        let mut words = format!("resets {}", at(week.reset));
        if let Some(began) = day.new_week() {
            words = format!("new week {}, {words}", clock(began, tz_at));
        }
        parts.push(format!("week {figures} ({words}){}", marked(WEEK_LIMIT)));
    }
    if let Some(five) = day.sum.as_ref().and_then(|s| s.five_hour.as_ref()) {
        let mut bits = Vec::new();
        match five.closed {
            0 => {}
            1 => bits.push("1 window".to_owned()),
            n => bits.push(format!("{n} windows")),
        }
        bits.push(format!("lowest {}", left(five.peak)));
        if five.walled > 0 {
            bits.push(format!("{} used up", five.walled));
        }
        if let Some(open) = day.open_x10 {
            bits.push(format!("open {}", left(open)));
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

fn day_json(day: &HistoryDay) -> Value {
    let mut o = Map::new();
    o.insert("date".to_owned(), Value::from(date_of(day.t, day.tz_s)));
    put_time(&mut o, "t", Some(day.t));
    o.insert("tz_s".to_owned(), Value::from(day.tz_s));
    let sum = day.sum.as_ref();
    o.insert(
        "five_hour".to_owned(),
        sum.and_then(|s| s.five_hour.as_ref())
            .map_or(Value::Null, |five| {
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
    /// The label `accounts stats` reads its records by ([`super::StatsHome::records`]); `None`
    /// for a home no row holds whose word a row is labelled with, which is no row's home.
    label: Option<String>,
    /// What its transcripts hold and the newest instant another account was seen on it
    /// ([`account_stats::another_seen`]), or why nothing is booked to it: `not-the-home` (no
    /// block of its label is made, so nothing is read), `several-logins` (the roster lists
    /// several, so its walls may be any of theirs), `no-home` (no Claude Code directory is
    /// known), `no-profile` (its profile could not be read, or names no account or no address:
    /// nothing says whose the home is, and no conversation's address can be compared with its
    /// own) or `another-account` (the label's record is another account's than the one the
    /// home's profile names).
    read: Result<(Walls, Option<i64>), &'static str>,
}

impl Home {
    /// The home `env` names, of `records`, read for a history over `span` where `wanted` says a
    /// block of its label is made: its transcripts last written since the span's start less a
    /// week (no close of a wall before then ends inside the span) and since another account was
    /// last seen on it, each conversation's address compared with the one its profile names
    /// (read for that alone, [`crate::harness::accounts_sync::read_default_profile_email`]),
    /// within [`account_backfill::READ_BYTES`] and [`super::STATS_FOLD_S`].
    fn of(
        env: &Env,
        records: &accounts::AccountRecords,
        span: Span,
        wanted: impl FnOnce(&str) -> bool,
    ) -> Home {
        let profile = home_profile(env);
        let key = profile.as_ref().ok().and_then(|p| p.key);
        let label = stats_home(env, key, &records.roster)
            .records(&records.roster)
            .map(str::to_owned);
        let read = label.as_deref().map_or(Err("not-the-home"), |label| {
            Self::look(env, records, span, (label, profile.as_ref().ok()), wanted)
        });
        Home { label, read }
    }

    /// What [`Home::of`] finds of the home `env` names, its label `label` and its `profile`
    /// where one was read.
    fn look(
        env: &Env,
        records: &accounts::AccountRecords,
        span: Span,
        (label, profile): (&str, Option<&crate::harness::profile::Profile>),
        wanted: impl FnOnce(&str) -> bool,
    ) -> Result<(Walls, Option<i64>), &'static str> {
        if !wanted(label) {
            return Err("not-the-home");
        }
        if records.roster.several() {
            return Err("several-logins");
        }
        let dir = env.claude_dir().ok_or("no-home")?;
        let profile = profile.filter(|p| p.key.is_some()).ok_or("no-profile")?;
        let address =
            crate::harness::accounts_sync::read_default_profile_email(env).ok_or("no-profile")?;
        let record = records.state.accounts.get(label);
        if record.is_some_and(|a| a.of_another(profile.key)) {
            return Err("another-account");
        }
        let seen = account_stats::another_seen(env.now, false, record, Some(profile));
        let since = span
            .from
            .saturating_sub(WEEK_WINDOW_S)
            .max(seen.unwrap_or(i64::MIN));
        let start = std::time::Instant::now();
        let limits = crate::harness::spend::Limits {
            bytes: account_backfill::READ_BYTES,
            until: start.checked_add(std::time::Duration::from_secs(super::STATS_FOLD_S)),
        };
        let walls =
            account_backfill::read(&dir.join("projects"), &address, since, &limits, env.now);
        Ok((walls, seen))
    }
}

/// What the backfill did for one account's block: how many closes it booked inside the span,
/// whether the transcripts were read whole (else what cut the read), or why it read and booked
/// nothing (the home's [`Home::read`], or `not-the-home` for an account other than the home's).
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
    series: Option<(Series, Vec<HistoryDay>, Booked)>,
}

impl Block {
    /// The account's object in `--json`: `ac` (its key), `slug` and `name` (each `null` where it
    /// has none; a row with no address has none, and only it has `label`, its one name), the lab
    /// model's `journal` and `lines` and, with a series, `span`, `series`, `days`, `backfill`
    /// (`{booked, complete, cut, why, closes}`: `complete` is `null` where `why` says nothing was
    /// read; each close as a series row would say it, `src: "backfill"`) and (with `--rows`)
    /// `rows`, the stored rows alone; without one, `series: false`.
    fn json(&self, span: Span, (machine, journal, rows): (&str, bool, bool)) -> Map {
        let mut o = Map::new();
        o.insert("ac".to_owned(), Value::from(self.who.of.pk.clone()));
        let names = self.who.names.as_ref();
        o.insert(
            "slug".to_owned(),
            Value::from(
                names
                    .map(|names| names.slug.clone())
                    .filter(|slug| !slug.is_empty()),
            ),
        );
        o.insert(
            "name".to_owned(),
            Value::from(names.and_then(|names| names.name.clone())),
        );
        if names.is_none() {
            o.insert("label".to_owned(), Value::from(self.who.of.label.clone()));
        }
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
        b.insert(
            "complete".to_owned(),
            match booked.why {
                Some(_) => Value::Null,
                None => Value::from(booked.cut.is_none()),
            },
        );
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
            let _ = writeln!(out, "{}{DOT}{}", self.who.shown(), cli.id());
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
            self.who.shown(),
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
    let (Some(path), Some(store)) = (env.accounts_roster_for(cli), env.accounts_store_for(cli))
    else {
        return refuse(err, &no_home_for(cli));
    };
    let Ok(records) = store.read_account_records(Some(&path), env.now) else {
        return refuse(err, "the account records could not be read consistently");
    };
    let lab = crate::harness::alab_roster::current();
    let lab = lab.as_deref();
    let whos: Vec<Who> = match name {
        Some(word) => match resolve(cli, word, &records, lab) {
            Ok(who) => vec![who],
            Err(why) => return refuse(err, &why),
        },
        None => records
            .roster
            .rows
            .iter()
            .map(|row| Who::of_row(cli, row, &records.state, lab))
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
                .of
                .label
                .as_deref()
                .filter(|_| cli == Cli::Claude)
                .and_then(|label| account_series::path_of(&env.state, label, machine));
            (who, own)
        })
        .collect();
    let elsewhere = if cli == Cli::Claude {
        let asked: Vec<(&HistoryWho, Option<&std::path::Path>)> = whos
            .iter()
            .map(|(who, own)| (&who.of, own.as_deref()))
            .collect();
        by_key(&account_series::dir_in(&env.state), machine, &asked)
    } else {
        vec![Vec::new(); whos.len()]
    };
    let ledger = store.tail(accounts::LEDGER_KEEP_ROWS).unwrap_or_default();
    let ledger_name = ledger_name(cli, &records, &ledger, lab);
    let seen = alab_seen::current(true);
    let flags = (machine, seen.is_some(), rows);
    // The home the environment names, read only where a block of its label is made.
    let home = (cli == Cli::Claude).then(|| {
        Home::of(env, &records, span, |label| {
            whos.iter()
                .any(|(who, _)| who.of.label.as_deref() == Some(label))
        })
    });
    let block_of = |who: Who, own: Option<PathBuf>, elsewhere: Vec<PathBuf>| {
        let journal = seen
            .as_deref()
            .zip(who.of.pk.as_deref())
            .and_then(|(seen, pk)| seen.accounts.get(pk));
        let lines = alab_seen::history(&who.asked(), &ledger, journal, &ledger_name);
        let spent = alab_seen::spent(&who.asked(), &ledger);
        let series = home.as_ref().map(|home| {
            let files: Vec<(PathBuf, bool)> = own
                .into_iter()
                .map(|path| (path, true))
                .chain(elsewhere.into_iter().map(|path| (path, false)))
                .collect();
            let mut series = gather(&files, &who.of, span, rows, env.now);
            let booked = match &home.read {
                _ if home.label.is_none() || who.of.label != home.label => Booked {
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
                        &series.files,
                        (*another_seen).max(series.taken_over),
                        ((who.of.pk.as_deref(), who.of.key), machine),
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
            let days = days_of(&series, &lines, &spent, span, tz_at, env.now);
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
            return refuse(err, "no account was named");
        };
        let block = block_of(who, own, elsewhere);
        if json {
            let mut doc = Map::new();
            doc.insert("kind".to_owned(), Value::from("account-history"));
            doc.insert("schema".to_owned(), Value::from(HISTORY_SCHEMA));
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
        doc.insert("schema".to_owned(), Value::from(HISTORY_SCHEMA));
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
