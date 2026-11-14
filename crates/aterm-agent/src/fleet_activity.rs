// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet activity`, and the same read for the menu-bar item's menu: this Mac's fleet
//! activity record (`aterm_types::fleet_activity`, `aterm_ctl::fleet_activity`) as a person
//! reads it.
//!
//! A row keeps the other Mac's name as its writer knew it then; a reader names each Mac as this
//! Mac's book names its node now ([`Names`]), and keeps the row's own name only for a node the
//! book no longer holds. The read is local: the record's file and the book, no network and no
//! other process.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_ctl::fleet_activity::{Record, Seen};
use aterm_json::{Value, json};
use aterm_types::fleet_activity::{
    Counts, Outcome, Pair, Plain, Row, Summary, WINDOW_MS, ago, span, summarize,
};

/// The shortest window `--since` takes, in seconds.
const SINCE_MIN_S: u64 = 60;

/// The longest window `--since` takes, in seconds: the age the record keeps a row to.
const SINCE_MAX_S: u64 = aterm_ctl::fleet_activity::MAX_AGE_MS / 1000;

const USAGE: &str = "aterm fleet activity [--json] [--since <span>]";

/// What this Mac's book calls each node now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names(Vec<(String, String)>);

impl Names {
    /// The names of the members the book in `state` binds a key to: each one's primary name,
    /// else the host its entry holds.
    #[must_use]
    pub fn of(state: &Path) -> Self {
        Self(
            aterm_net::fleet_member::bound_peers(state)
                .into_iter()
                .filter_map(|peer| {
                    let name = peer
                        .primary_name()
                        .map_or_else(|| peer.host.clone(), str::to_owned);
                    Some((peer.node?, name))
                })
                .collect(),
        )
    }

    /// The name of `node`, where the book holds it.
    #[must_use]
    pub fn of_node(&self, node: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(held, _)| held == node)
            .map(|(_, name)| name.as_str())
    }

    /// `rows` with each Mac named as the book names it now; a row of a node the book does not
    /// hold keeps the name it was written with.
    #[must_use]
    pub fn naming(&self, mut rows: Vec<Row>) -> Vec<Row> {
        for row in &mut rows {
            if let Some(name) = self.of_node(&row.node) {
                row.host = Some(name.to_owned());
            }
        }
        rows
    }
}

/// The record of the state folder `state` as a reader that looks again and again holds it: the
/// menu-bar item's background reader calls [`Self::summary`] on its tick and puts the answer in
/// the slot its menu copies. The record's file is read only when it changed since the last
/// look, and the book's only when it did; every look adds the held rows up again, since a row
/// leaves the window, and the record, by the clock alone.
#[derive(Debug)]
pub struct Watch {
    state: PathBuf,
    record: Record,
    seen: Option<Seen>,
    /// The book's file as it stood when the names were last read from it.
    book: BookSeen,
    names: Names,
    /// The rows as the record's file gave them, each with the name it was written with.
    read: Vec<Row>,
    /// What [`Self::read`] holds, each Mac named as the book names it now.
    rows: Vec<Row>,
    cut_ms: Option<u64>,
}

/// The book's two files (this Mac's own and its copy of the issuer's) as a [`Watch`] last read
/// the names from them: which file each was and when it was last written, `None` for one that
/// is not there.
type BookSeen = [Option<(aterm_uds::line_log::Identity, Option<std::time::SystemTime>)>; 2];

impl Watch {
    /// A reader of the record in `state`, which has read nothing yet.
    #[must_use]
    pub fn in_state(state: &Path) -> Self {
        Self {
            state: state.to_path_buf(),
            record: Record::in_state(state),
            seen: None,
            book: [None; 2],
            names: Names::default(),
            read: Vec::new(),
            rows: Vec::new(),
            cut_ms: None,
        }
    }

    /// The record's rows as last read, oldest first, each Mac named as the book named it then.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// What the `window_ms` before `now_ms` adds up to: the answer `aterm fleet activity` gives
    /// at the same instant ([`read`]). The record is read again where it was written since the
    /// last call and the names where the book was; a held row the record no longer keeps at
    /// `now_ms` (older than its age bound) is dropped, and one stamped from before the clock
    /// was set back is settled once ([`aterm_types::fleet_activity::settled`]).
    pub fn summary(&mut self, now_ms: u64, window_ms: u64) -> Summary {
        let mut moved = false;
        if let Some(kept) = self.record.read_new(&mut self.seen, now_ms) {
            self.read = kept.rows;
            self.cut_ms = kept.cut_ms;
            moved = true;
        }
        let before = self.read.len();
        self.read.retain(|row| {
            now_ms.saturating_sub(row.at_ms) <= aterm_ctl::fleet_activity::MAX_AGE_MS
        });
        moved |= self.read.len() != before;
        moved |= aterm_types::fleet_activity::settled(&mut self.read, now_ms);
        let book = book_seen(&self.state);
        if book != self.book {
            self.names = Names::of(&self.state);
            self.book = book;
            moved = true;
        }
        if moved {
            self.rows = self.names.naming(self.read.clone());
        }
        summarize(&self.rows, now_ms, window_ms).since(self.cut_ms, now_ms)
    }
}

/// The book's files in `state` as they stand ([`BookSeen`]).
fn book_seen(state: &Path) -> BookSeen {
    use aterm_net::fleet_member::{ISSUER_BOOK_FILE, PEERS_FILE};
    [PEERS_FILE, ISSUER_BOOK_FILE].map(|file| {
        let meta = std::fs::symlink_metadata(state.join(file)).ok()?;
        Some((
            aterm_uds::line_log::identity_of(&meta),
            meta.modified().ok(),
        ))
    })
}

/// The record of `state` read once at `now_ms`: its rows oldest first, named by the book, and
/// what the `window_ms` before `now_ms` adds up to.
#[must_use]
pub fn read(state: &Path, now_ms: u64, window_ms: u64) -> (Vec<Row>, Summary) {
    let kept = Record::in_state(state).read(now_ms);
    let rows = Names::of(state).naming(kept.rows);
    let summary = summarize(&rows, now_ms, window_ms).since(kept.cut_ms, now_ms);
    (rows, summary)
}

/// A Mac as a line names it: its name, else the head of its node id.
fn mac<'a>(host: Option<&'a str>, node: &'a str) -> &'a str {
    host.unwrap_or_else(|| node.get(..8).unwrap_or(node))
}

/// `at_ms` as an RFC 3339 instant, to the second.
fn instant(at_ms: u64) -> String {
    aterm_types::rfc3339::format_rfc3339(at_ms / 1000)
}

/// `lines` with every column but the last padded to its widest cell, two spaces apart.
fn columns(lines: &[Vec<String>]) -> String {
    let cells = lines.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cells)
        .map(|at| {
            lines
                .iter()
                .filter_map(|line| line.get(at))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for line in lines {
        let mut text = String::new();
        for (at, cell) in line.iter().enumerate() {
            text.push_str(cell);
            if at + 1 < line.len() {
                let pad = widths[at].saturating_sub(cell.chars().count()) + 2;
                text.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

/// One row's words after its time: which way, the Mac, the type (with its count where it
/// stands for several), what the lock decided, and the session that asked.
fn row_cells(row: &Row) -> Vec<String> {
    let what = if row.n > 1 {
        format!("{} x{}", row.what.word(), row.n)
    } else {
        row.what.word().to_owned()
    };
    let outcome = match row.outcome {
        Outcome::Admitted => "admitted".to_owned(),
        Outcome::Blocked { level, needs } => format!("blocked at {level}, needs {needs}"),
    };
    vec![
        instant(row.at_ms),
        row.way.arrow().to_owned(),
        mac(row.host.as_deref(), &row.node).to_owned(),
        what,
        outcome,
        row.by.clone().unwrap_or_default(),
    ]
}

/// The text `aterm fleet activity` prints: the summary's one line, a line for each Mac and way
/// in the window (busiest first), and under a blank line the window's rows, newest first.
#[must_use]
pub fn render(rows: &[Row], summary: &Summary, now_ms: u64) -> String {
    let mut out = summary.headline(now_ms);
    out.push('\n');
    let pairs: Vec<Vec<String>> = summary
        .pairs
        .iter()
        .map(|pair| {
            vec![
                pair.name().to_owned(),
                pair.way.arrow().to_owned(),
                pair.count().to_string(),
                pair.phrase(),
                ago(now_ms, pair.last_ms),
            ]
        })
        .collect();
    out.push_str(&columns(&pairs));
    let window: Vec<Vec<String>> = in_window(rows, summary, now_ms).map(row_cells).collect();
    if !window.is_empty() {
        out.push('\n');
        out.push_str(&columns(&window));
    }
    out
}

/// The rows of `summary`'s window, newest first.
fn in_window<'a>(rows: &'a [Row], summary: &Summary, now_ms: u64) -> impl Iterator<Item = &'a Row> {
    let window_ms = summary.window_ms;
    let mut window: Vec<&Row> = rows
        .iter()
        .filter(|row| now_ms.saturating_sub(row.at_ms) < window_ms)
        .collect();
    window.sort_by_key(|row| std::cmp::Reverse(row.at_ms));
    window.into_iter()
}

fn counts_json(counts: &Counts) -> Value {
    let mut types = aterm_json::Map::new();
    for what in Plain::ALL {
        types.insert(what.word().to_owned(), Value::from(counts.of(what)));
    }
    Value::Object(types)
}

fn name_json(host: Option<&str>) -> Value {
    host.map_or(Value::Null, Value::from)
}

fn pair_json(pair: &Pair) -> Value {
    json!({
        "node": pair.node.as_str(),
        "host": name_json(pair.host.as_deref()),
        "way": pair.way.word(),
        "count": pair.count(),
        "types": counts_json(&pair.types),
        "blocked": counts_json(&pair.blocked),
        "phrase": pair.phrase(),
        "last_ms": pair.last_ms,
        "routine": pair.routine(),
    })
}

fn row_json(row: &Row) -> Value {
    let (outcome, level, needs) = match row.outcome {
        Outcome::Admitted => ("admitted", Value::Null, Value::Null),
        Outcome::Blocked { level, needs } => (
            "blocked",
            Value::from(level.word()),
            Value::from(needs.word()),
        ),
    };
    json!({
        "at_ms": row.at_ms,
        "at": instant(row.at_ms),
        "way": row.way.word(),
        "node": row.node.as_str(),
        "host": name_json(row.host.as_deref()),
        "type": row.what.word(),
        "n": row.n,
        "outcome": outcome,
        "level": level,
        "needs": needs,
        "by": row.by.as_deref().map_or(Value::Null, Value::from),
    })
}

/// What `aterm fleet activity --json` prints: the summary, each Mac and way in the window, and
/// the window's rows, newest first.
#[must_use]
pub fn to_json(rows: &[Row], summary: &Summary, now_ms: u64) -> Value {
    let macs = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    json!({
        "now_ms": now_ms,
        "window_ms": summary.window_ms,
        "window": span(summary.window_ms),
        "headline": summary.headline(now_ms),
        "requests": summary.count(),
        "macs_in": macs(summary.peers_in),
        "macs_out": macs(summary.peers_out),
        "blocked_in": summary.blocked_in,
        "blocked_out": summary.blocked_out,
        "last_blocked_in_ms": summary.last_blocked_in_ms.map_or(Value::Null, Value::from),
        "last_ms": summary.last_ms.map_or(Value::Null, Value::from),
        "cut_ms": summary.cut_ms.map_or(Value::Null, Value::from),
        "types": counts_json(&summary.types),
        "pairs": summary.pairs.iter().map(pair_json).collect::<Vec<_>>(),
        "rows": in_window(rows, summary, now_ms).map(row_json).collect::<Vec<_>>(),
    })
}

/// What `aterm fleet activity`'s words ask for: whether JSON, and the window in milliseconds.
fn parse(args: &[String]) -> Result<(bool, u64), String> {
    let (mut json, mut since) = (false, None);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let value = match word.as_str() {
            "--json" if !json => {
                json = true;
                continue;
            }
            "--since" => words.next().map(String::as_str),
            other => other.strip_prefix("--since="),
        };
        let Some(value) = value.filter(|_| since.is_none()) else {
            return Err(format!(
                "unexpected `{}` ({USAGE})",
                aterm_codec::tty::one_line(word, 40)
            ));
        };
        since = Some(crate::harness::cli::parse_span(
            "--since",
            value,
            SINCE_MIN_S,
            SINCE_MAX_S,
        )?);
    }
    Ok((json, since.map_or(WINDOW_MS, |secs| secs * 1000)))
}

/// What `aterm fleet activity <args>` answers over the record of `state` at `now_ms`.
fn answer(state: &Path, args: &[String], now_ms: u64) -> Result<String, String> {
    let (json, window_ms) = parse(args)?;
    let (rows, summary) = read(state, now_ms, window_ms);
    Ok(if json {
        let mut text = aterm_json::to_string(&to_json(&rows, &summary, now_ms)).unwrap_or_default();
        text.push('\n');
        text
    } else {
        render(&rows, &summary, now_ms)
    })
}

/// `aterm fleet activity [--json] [--since <span>]`: the answer on stdout, exit 0; a usage
/// error exits 2.
pub(crate) fn main_entry(args: &[std::ffi::OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return aterm_uds::stdout::answer_exit(
            "aterm fleet activity",
            crate::fleet_member::USAGE,
            0,
        );
    }
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm fleet activity: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    match answer(&state, &args, aterm_ctl::fleet_activity::now_ms()) {
        Ok(text) => aterm_uds::stdout::answer_exit("aterm fleet activity", &text, 0),
        Err(why) => {
            eprintln!("aterm fleet activity: {why}\n(`aterm fleet --help` lists the commands)");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
#[path = "fleet_activity_tests.rs"]
mod tests;
