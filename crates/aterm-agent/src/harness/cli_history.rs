// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts history [<label|key>] [--since <span>] [--rows] [--json]`: one
//! account's days from this Mac's readings series (`docs/DESIGN-account-stats-2026-10-15.md` §5),
//! one line a local day, with that day's ledger rows and every Mac's journal events about it
//! beneath ([`alab_seen::history`]); with no name, every roster row's, in roster order. Only the
//! account's own readings count: its label's series and, by its keys, any other of this Mac's
//! series that holds its rows (a renamed row's), each row judged by the keys it carries
//! (the fold is [`crate::harness::account_history`]'s, which the Account card shares). On a Mac whose roster has one login, the walls its home's transcripts record of
//! windows the series holds no word of join the home's row's days ([`account_backfill`], marked
//! `(backfill)`). It stores nothing of its own: no compaction runs and no derived or backfilled
//! row is kept. Nothing calls a provider, and neither a ledger row's free text nor a transcript's
//! text is printed.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::{Cli, Env, home_profile, no_home_for, put_time, stats_label};
use crate::harness::account_backfill::{self, Cut, Walls};
use crate::harness::account_history::{
    Happened, HistoryDay, HistoryWho, RESET_USED, Series, Span, by_key, days_of, gather,
};
use crate::harness::account_series::{self, SeriesRow};
use crate::harness::account_stats::{self, day_of, hm, span as span_word};
use crate::harness::accounts::{self, SESSION_LIMIT, WEEK_LIMIT, WEEK_WINDOW_S};
use crate::harness::alab_seen;
use crate::harness::profile::AccountKey;

const DOT: &str = " \u{b7} ";
const DAY_S: i64 = 86_400;

/// The account a block is about ([`HistoryWho`]: its row's label here, its lab key and its
/// account key, as many as are known) and the name the block goes by.
#[derive(Debug, Clone)]
struct Who {
    of: HistoryWho,
    name: String,
}

impl Who {
    fn asked(&self) -> alab_seen::Asked {
        alab_seen::Asked {
            label: self.of.label.clone(),
            pk: self.of.pk.clone(),
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
            of: HistoryWho {
                label: asked.label,
                pk: asked.pk,
                key,
            },
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
        of: HistoryWho {
            label,
            pk,
            key: Some(key),
        },
    })
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

/// One day's line: `Sat Nov 7  week 42% → 29% left (resets Sun Nov 8 20:50) · 5h 3 windows,
/// lowest 0% left, 1 used up · reset used 14:10`; the five-hour lowest is what was left at the
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
    /// The label `accounts stats` names it by ([`stats_label`]).
    label: String,
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
        let label = stats_label(env, key, &records.roster);
        let read = Self::look(env, records, span, (&label, profile.as_ref().ok()), wanted);
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
    /// Whether the block holds nothing to go by: no row, no lab key and no series row.
    fn is_unknown(&self) -> bool {
        self.who.of.label.is_none()
            && self.who.of.pk.is_none()
            && self
                .series
                .as_ref()
                .is_none_or(|(s, _, _)| s.files.is_empty() && s.backfill.is_empty())
    }

    /// The account's object in `--json`: the lab model's `{label, ac, journal, lines}` and, with
    /// a series, `span`, `series`, `days`, `backfill` (`{booked, complete, cut, why, closes}`:
    /// `complete` is `null` where `why` says nothing was read; each close as a series row would
    /// say it, `src: "backfill"`) and (with `--rows`) `rows`, the stored rows alone; without one,
    /// `series: false`.
    fn json(&self, span: Span, (machine, journal, rows): (&str, bool, bool)) -> Map {
        let mut o = Map::new();
        o.insert("label".to_owned(), Value::from(self.who.of.label.clone()));
        o.insert("ac".to_owned(), Value::from(self.who.of.pk.clone()));
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
                    of: HistoryWho {
                        label: Some(row.label.clone()),
                        key: key_in(&records.state, Some(&row.label), pk.as_deref()),
                        pk,
                    },
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
        let lines = alab_seen::history(&who.asked(), &ledger, journal);
        let series = home.as_ref().map(|home| {
            let files: Vec<(PathBuf, bool)> = own
                .into_iter()
                .map(|path| (path, true))
                .chain(elsewhere.into_iter().map(|path| (path, false)))
                .collect();
            let mut series = gather(&files, &who.of, span, rows, env.now);
            let booked = match &home.read {
                _ if who.of.label.as_deref() != Some(home.label.as_str()) => Booked {
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
