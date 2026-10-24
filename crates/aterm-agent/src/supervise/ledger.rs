// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive ledger`: multi-source timeline joining worker turn history, watcher journal, fabric mail, and reply row counts.
//!
//! The session log places every time by default ([`LedgerSource::Log`]): each turn by its
//! record's `t=` joined by `ledger=`, each message by its `mail` mark joined by `inbox=`.
//! [`LedgerSource::Anchor`] (`--source anchor`) is deprecated: it is kept until the release
//! after the one that ships the log default, and removed then.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use super::journal::{JournalRecord, Noted, Word, json_str, read_journal};
use super::limit::{civil, days_from_civil};
use super::report::{
    MARKER_CHARS, Mark, OffscreenError, PASTED, decimal, is_user_row, join, parse_offscreen,
    paste_fits, paste_like, pct_decode, user_text, without_verb,
};
use super::run::{Ctl, CtlReply};

/// `key=value` of a row's words.
fn field<'r>(row: &'r str, key: &str) -> Option<&'r str> {
    row.split(' ')
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// A sid as the mail rows spell it (`@s-…`, `s-…@n-…`), bare.
fn bare_sid(s: &str) -> &str {
    let s = s.trim_start_matches('@');
    s.split_once('@').map_or(s, |(sid, _)| sid)
}

/// Whether `from`, a row's attested sender, is the session `worker` names
/// (`@s-…`/`s-…`, or `@s-…@n-…` for a session on another node) — the WHOLE
/// principal, never the sid alone.
fn is_from_session(from: &str, worker: &str) -> bool {
    let worker = worker.strip_prefix('@').unwrap_or(worker);
    let Some((sid, node)) = from.split_once('@') else {
        return false;
    };
    if !node.starts_with("n-") {
        return false;
    }
    match worker.split_once('@') {
        Some((want_sid, want_node)) => sid == want_sid && node == want_node,
        None => sid == worker,
    }
}

/// Maximum archived rows requested when measuring turn reply sizes.
pub const LEDGER_MAX_ROWS: usize = 20_000;
/// How many characters of a turn's text the timeline shows.
const TURN_CHARS: usize = 100;
/// How wide the text timeline's WHAT column is.
const WHAT_CHARS: usize = 96;

/// Output format for `aterm drive ledger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Aligned columns for a terminal (the default).
    #[default]
    Text,
    /// Markdown tables.
    Md,
    /// One self-contained HTML page: swimlanes on a time axis.
    Html,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Format::Text),
            "md" | "markdown" => Some(Format::Md),
            "html" => Some(Format::Html),
            _ => None,
        }
    }
}

/// Options for [`gather`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerOpts {
    /// The worker, as `@s-…`.
    pub worker: String,
    /// `--journal`: the watcher's journal.
    pub journal: Option<PathBuf>,
    /// `--since`: nothing before this Unix time (ms).
    pub since_ms: Option<i64>,
    /// `--source`: where the times come from.
    pub source: LedgerSource,
}

/// Clock source used to place turns and mail on the wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LedgerSource {
    /// Session-log wall-clock timestamps joined by `ledger=` and `inbox=` (the default).
    #[default]
    Log,
    /// Process-clock timestamps anchored via [`ClockAnchor`]: deprecated, kept until the
    /// release after the one that ships the log default, and removed then.
    Anchor,
}

/// What the SOURCES clock row says of `--source anchor` whenever it is read.
pub const ANCHOR_DEPRECATED: &str = "`--source anchor` is deprecated: kept until the release \
                                     after the one that ships the log default, removed then";

impl LedgerSource {
    /// Parse `"log"` or `"anchor"`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "log" => Some(Self::Log),
            "anchor" => Some(Self::Anchor),
            _ => None,
        }
    }
}

/// Wall-clock anchor for an aterm instance's monotonic process clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockAnchor {
    /// The instance's pid.
    pub pid: u32,
    /// The Unix time (ms) of its clock's zero.
    pub epoch_ms: i64,
    /// How that was found, for SOURCES.
    pub how: String,
}

/// Host clock and process-anchor callback for [`gather`].
pub struct LedgerHost<'a> {
    /// The time now, Unix ms.
    pub now_ms: i64,
    /// The local time's offset from UTC, seconds (times print in it).
    pub tz_offset_s: i64,
    /// Where the process hosting a session (by sid, no `@`) started its
    /// clock.
    pub anchor: &'a mut dyn FnMut(&str) -> Result<ClockAnchor, String>,
}

/// Diagnostic status of one ledger data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Parsed record from a session `history` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistTurn {
    pub id: u64,
    pub submitted: bool,
    pub status: String,
    /// Who typed it (`by=`: `harness`, `ctl`, `bridge`, a sid, `-`); `-` from
    /// a host that predates the field.
    pub by: String,
    pub started_ms: u64,
    pub dur_ms: u64,
    pub arch: Option<Mark>,
    pub carried: bool,
    pub text: String,
}

/// Extract `unheld_below=<id>` from a `history` reply header, if present.
pub(crate) fn history_floor(reply: &CtlReply) -> Option<u64> {
    let header = reply.stdout.lines().next().filter(|l| l.starts_with("OK "));
    header
        .into_iter()
        .chain(reply.stderr.lines())
        .flat_map(str::split_whitespace)
        .find_map(|word| word.strip_prefix("unheld_below=")?.parse().ok())
}

/// Parse `turn` records from a `history` reply body.
pub fn parse_history_records(body: &str) -> Vec<HistTurn> {
    body.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("turn ")?;
            let (head, text) = rest.split_once(" text=").unwrap_or((rest, ""));
            let mut words = head.split(' ');
            let mut t = HistTurn {
                id: decimal(words.next()?)?,
                submitted: false,
                status: "-".to_string(),
                by: "-".to_string(),
                started_ms: 0,
                dur_ms: 0,
                arch: None,
                carried: false,
                text: pct_decode(text),
            };
            for word in words {
                match word.split_once('=') {
                    Some(("submitted", v)) => t.submitted = v == "1",
                    Some(("status", v)) => t.status = v.to_string(),
                    Some(("by", v)) => t.by = pct_decode(v),
                    Some(("started_ms", v)) => t.started_ms = decimal(v).unwrap_or(0),
                    Some(("dur_ms", v)) => t.dur_ms = decimal(v).unwrap_or(0),
                    Some(("arch", v)) => t.arch = Mark::parse(v),
                    Some(("carried", v)) => t.carried = v == "1",
                    _ => {}
                }
            }
            Some(t)
        })
        .collect()
}

/// Direction of a fabric message relative to the manager session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// From the worker to this session.
    In,
    /// From this session to the worker.
    Out,
}

/// Fabric message exchanged between the manager and the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    /// Aligned Unix ms (`None` without a clock).
    pub t: Option<i64>,
    pub dir: Dir,
    pub kind: String,
    pub len: Option<u64>,
    pub trust: Option<String>,
    /// An outbound post that has not landed on the bus yet.
    pub pending: bool,
}

/// The `inbox --peek --meta` rows from `worker` (`msg <id> … t=<ms>
/// from=<sid@node> kind=… trust=… len=…`, the whole principal:
/// [`is_from_session`]) with their id and process-clock `t`, and the ids of
/// the posts to it that have not landed (`post <id> to=… off=-`).
fn inbox_rows(body: &str, worker: &str) -> (Vec<(u64, u64, Mail)>, Vec<u64>) {
    let mut got = Vec::new();
    let mut pending = Vec::new();
    for row in body.lines() {
        if row.starts_with("msg ") {
            if !field(row, "from").is_some_and(|from| is_from_session(from, worker)) {
                continue;
            }
            let t = field(row, "t").and_then(decimal);
            let id = row.split(' ').nth(1).and_then(decimal).unwrap_or(0);
            got.push((
                id,
                t.unwrap_or(0),
                Mail {
                    t: None,
                    dir: Dir::In,
                    kind: field(row, "kind").unwrap_or("-").to_string(),
                    len: field(row, "len").and_then(decimal),
                    trust: field(row, "trust").map(str::to_string),
                    pending: false,
                },
            ));
        } else if let Some(rest) = row.strip_prefix("post ")
            && field(row, "to").map(bare_sid) == Some(bare_sid(worker))
            && let Some(id) = rest.split(' ').next().and_then(decimal)
        {
            pending.push(id);
        }
    }
    (got, pending)
}

/// The `timeline` rows that posted to `worker` (`event <n> t=<ms> kind=post
/// <id> to=<@sid> kind=<k>`): process-clock `t`, post id, message kind.
fn timeline_posts(body: &str, worker: &str) -> Vec<(u64, u64, String)> {
    body.lines()
        .filter_map(|row| {
            let rest = row.strip_prefix("event ")?;
            let (_, rest) = rest.split_once(' ')?;
            let (t, rest) = rest.split_once(' ')?;
            let t = decimal(t.strip_prefix("t=")?)?;
            let rest = rest.strip_prefix("kind=post ")?;
            let (id, rest) = rest.split_once(' ')?;
            if field(rest, "to").map(bare_sid) != Some(worker) {
                return None;
            }
            Some((
                t,
                decimal(id)?,
                field(rest, "kind").unwrap_or("-").to_string(),
            ))
        })
        .collect()
}

/// One record line of a `log` reply (`<seq> <op> <node> <kind> [par=…]
/// <key>=<value>… t=<ms>`): its op, node and kind words, and its `t=`.
fn log_record(line: &str) -> Option<(&str, &str, &str, Option<i64>)> {
    let mut w = line.split(' ');
    decimal(w.next()?)?;
    let (op, node, kind) = (w.next()?, w.next()?, w.next()?);
    let t = field(line, "t")
        .and_then(decimal)
        .and_then(|t| i64::try_from(t).ok());
    Some((op, node, kind, t))
}

/// What the worker's `log class=turn` says of its turns,
/// by `ledger=` alone, never by time.
#[derive(Debug, Default, PartialEq, Eq)]
struct LogTurns {
    /// Each `ledger=` id's turn start, wall ms: the turn's open record's
    /// `t=`, or — its open evicted — its close's `t=` less `dur_ms` (the
    /// close restates `ledger=`, 5F-4a).
    start: BTreeMap<u64, i64>,
    /// Of `start`, the turns placed by their close: the open was evicted.
    by_close: usize,
    /// Turns only a close shows (the open evicted) that name no `ledger=`.
    unledgered: usize,
    /// The records the ring evicted before this read (its `gap` line).
    lost: u64,
}

/// [`LogTurns`] from a `log class=turn` reply's body.
fn log_turns(body: &str) -> LogTurns {
    let mut out = LogTurns::default();
    let mut opened: Vec<&str> = Vec::new();
    for line in body.lines() {
        if let Some(gap) = line.strip_prefix("gap ") {
            out.lost += field(gap, "lost").and_then(decimal).unwrap_or(0);
            continue;
        }
        let Some((op, node, kind, t)) = log_record(line) else {
            continue;
        };
        if kind != "turn" {
            continue;
        }
        let ledger = field(line, "ledger").and_then(decimal);
        match op {
            "open" => {
                opened.push(node);
                if let (Some(id), Some(t)) = (ledger, t) {
                    out.start.entry(id).or_insert(t);
                }
            }
            "close" if !opened.contains(&node) => match (ledger, t) {
                (Some(id), Some(t)) => {
                    let dur = field(line, "dur_ms")
                        .and_then(decimal)
                        .and_then(|d| i64::try_from(d).ok())
                        .unwrap_or(0);
                    if let std::collections::btree_map::Entry::Vacant(e) = out.start.entry(id) {
                        e.insert(t.saturating_sub(dur));
                        out.by_close += 1;
                    }
                }
                _ => out.unledgered += 1,
            },
            _ => {}
        }
    }
    out
}

/// One post to the worker a `mail dir=out` mark tells: its wall time, post
/// id and kind.
type LogPost = (i64, u64, String);

/// The `mail` marks of this session's `log kinds=mail` reply: each message
/// in's wall time by its inbox id (`dir=in … inbox=<id>`), and each post to
/// `worker` (`dir=out to=<@sid> kind=<k> inbox=<post id>`).
fn log_mail(body: &str, worker: &str) -> (BTreeMap<u64, i64>, Vec<LogPost>) {
    let mut ins = BTreeMap::new();
    let mut outs = Vec::new();
    for line in body.lines() {
        let Some((op, _, kind, Some(t))) = log_record(line) else {
            continue;
        };
        let Some(id) = field(line, "inbox").and_then(decimal) else {
            continue;
        };
        if op != "mark" || kind != "mail" {
            continue;
        }
        match field(line, "dir") {
            Some("in") => {
                ins.entry(id).or_insert(t);
            }
            Some("out")
                if field(line, "to").map(pct_decode).as_deref().map(bare_sid) == Some(worker) =>
            {
                outs.push((t, id, field(line, "kind").unwrap_or("-").to_string()));
            }
            _ => {}
        }
    }
    (ins, outs)
}

/// Manager turn aligned onto the wall clock with its reply row count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub id: u64,
    /// Aligned Unix ms of its start (`None`: carried, or no clock).
    pub t: Option<i64>,
    pub status: String,
    pub submitted: bool,
    pub dur_ms: u64,
    pub carried: bool,
    /// Whether `t` is when the worker's session log saw the turn BEGIN (its
    /// open record, `--source log`), which is after the press, rather than
    /// when the exchange that typed it started (`history`'s `started_ms`).
    pub began: bool,
    pub text: String,
    /// The worker's reply, in rows, or why it is not known.
    pub rows: Result<usize, String>,
    /// The turns whose own `❯` row was not found, so their replies are
    /// inside this one's count.
    pub swallowed: Vec<u64>,
    /// The journal's first EVENT that ended on words (idle, question,
    /// limited) after it started and before the next turn: when the worker
    /// stopped.
    pub stopped: Option<i64>,
}

/// Assembled multi-source timeline for a supervised worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    /// The worker's sid (no `@`).
    pub worker: String,
    /// This session's sid (`whoami`), when it answered.
    pub manager: Option<String>,
    pub now_ms: i64,
    pub tz_offset_s: i64,
    pub since_ms: Option<i64>,
    pub sources: Vec<Source>,
    pub turns: Vec<TurnRow>,
    /// The journal's records for this worker, in order.
    pub journal: Vec<JournalRecord>,
    /// Whether a journal was read at all.
    pub journaled: bool,
    /// A `--journal` was given and could not be read: the counts it would have filled say so
    /// instead of `0`.
    pub journal_unreadable: bool,
    pub mail: Vec<Mail>,
    /// Whether the turns are placed on the wall clock (the session log read, or the deprecated
    /// anchor found): the manager's latency is measured only then.
    pub aligned: bool,
    /// Whether the times shown are guessed: only `--source anchor` with no anchor, which
    /// places its times as if the newest record were now, each printed after a `~`. The log's
    /// times, the journal's and the mail marks' are wall times, so the default never guesses.
    pub guessed: bool,
}

/// One control request's reply, or the reason there is none.
fn ask<C: Ctl>(ctl: &mut C, args: &[&str]) -> Result<CtlReply, String> {
    let r = ctl.call(args)?;
    if r.ok() {
        Ok(r)
    } else if without_verb(&r) {
        Err(format!(
            "the host has no `{}`",
            args.iter().find(|a| !a.starts_with('@')).unwrap_or(&"")
        ))
    } else {
        Err(one_line(r.stderr.trim()))
    }
}

/// `s` on one line, control characters as spaces.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The first `n` characters of `s` on one line, `…` when cut.
fn cut(s: &str, n: usize) -> String {
    let flat = one_line(s);
    if flat.chars().count() <= n {
        flat
    } else {
        let mut out: String = flat.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Gather turn history, mail, reply sizes, and optional journal records into a [`Ledger`].
pub fn gather<C: Ctl>(ctl: &mut C, opts: &LedgerOpts, host: &mut LedgerHost<'_>) -> Ledger {
    let worker = bare_sid(&opts.worker).to_string();
    let at = format!("@{worker}");
    let mut sources = Vec::new();

    let manager = ask(ctl, &["whoami"])
        .ok()
        .and_then(|r| r.stdout.split_whitespace().nth(1).map(str::to_string));
    let from_log = opts.source == LedgerSource::Log;
    let worker_anchor = if from_log {
        Err("the session log's own times are read instead".to_string())
    } else {
        (host.anchor)(&worker)
    };
    let manager_anchor = if from_log {
        None
    } else {
        manager.as_deref().map(|m| (host.anchor)(m))
    };
    let mut aligned = worker_anchor.is_ok();
    let guessed = !from_log && worker_anchor.is_err();
    sources.push(Source {
        name: "clock",
        ok: aligned || from_log,
        detail: match &worker_anchor {
            _ if from_log => "the session log's own wall times: each turn by its record's t= \
                              joined by ledger=, each message by its mail mark's joined by \
                              inbox=; no clock anchor is read"
                .to_string(),
            Ok(a) => format!(
                "{ANCHOR_DEPRECATED}; instance {}'s clock started {} ({}); history, inbox and \
                 timeline times are placed by it",
                a.pid,
                stamp(a.epoch_ms, host.tz_offset_s, true),
                a.how
            ),
            Err(e) => format!(
                "{ANCHOR_DEPRECATED}; not placed on the wall clock ({e}): worker and mail times \
                 are shown as if its newest record were now, and no latency is measured"
            ),
        },
    });

    let mut hist: Vec<HistTurn> = Vec::new();
    let mut newest = None;
    match ask(ctl, &[&at, "history"]) {
        Ok(r) => {
            hist = parse_history_records(&r.stdout);
            newest = hist
                .iter()
                .filter(|t| !t.carried)
                .map(|t| t.started_ms)
                .max();
            let typed = hist.len();
            hist.retain(|t| t.by != "harness");
            let harness = match typed - hist.len() {
                0 => String::new(),
                n => format!(
                    "; {n} more typed by aterm's window harness (by=harness), not the manager: \
                     left out"
                ),
            };
            let carried = hist.iter().filter(|t| t.carried).count();
            let lost = history_floor(&r).map(|floor| {
                format!(
                    "an aterm update could not carry its ledger whole: turns before id {floor} \
                     may be lost"
                )
            });
            sources.push(Source {
                name: "history",
                ok: !hist.is_empty(),
                detail: match (hist.is_empty(), lost) {
                    (true, Some(lost)) => {
                        format!("{at} has no turns in its ledger ({lost}){harness}")
                    }
                    (true, None) => {
                        format!("{at} has no turns in its ledger (typed by hand?){harness}")
                    }
                    (false, lost) => format!(
                        "{} turn(s) from {at}{}{}{harness}",
                        hist.len(),
                        match carried {
                            0 => String::new(),
                            _ if from_log => format!(", {carried} carried from an earlier aterm"),
                            _ => format!(", {carried} carried from an earlier aterm (no time)"),
                        },
                        lost.map(|lost| format!("; {lost}")).unwrap_or_default()
                    ),
                },
            });
        }
        Err(e) => sources.push(Source {
            name: "history",
            ok: false,
            detail: format!("{at}: {e}"),
        }),
    }

    let log_starts = from_log.then(|| match ask(ctl, &[&at, "log", "class=turn"]) {
        Ok(r) => {
            let lt = log_turns(&r.stdout);
            let placed = hist.iter().filter(|h| lt.start.contains_key(&h.id)).count();
            let mut detail = format!(
                "{at} log class=turn: {placed} of {} turn(s) placed by ledger=",
                hist.len()
            );
            if lt.by_close > 0 {
                let _ = write!(
                    detail,
                    " ({} by its close: the open was evicted)",
                    lt.by_close
                );
            }
            if placed < hist.len() {
                let _ = write!(
                    detail,
                    "; {} the log no longer holds: no time",
                    hist.len() - placed
                );
            }
            if lt.unledgered > 0 {
                let _ = write!(
                    detail,
                    "; {} closed turn(s) whose open was evicted name no ledger= (no `turn` \
                     exchange opened them, or an older aterm closed them): joined to nothing",
                    lt.unledgered
                );
            }
            if lt.lost > 0 {
                let _ = write!(
                    detail,
                    "; the ring evicted {} record(s) before this read",
                    lt.lost
                );
            }
            aligned = true;
            sources.push(Source {
                name: "log",
                ok: true,
                detail,
            });
            lt.start
        }
        Err(e) => {
            sources.push(Source {
                name: "log",
                ok: false,
                detail: format!("{at}: {e}: no turn is placed"),
            });
            BTreeMap::new()
        }
    });

    let fallback = host.now_ms - i64::try_from(newest.unwrap_or(0)).unwrap_or(0);
    let worker_epoch = worker_anchor.as_ref().map_or(fallback, |a| a.epoch_ms);
    let manager_epoch = match &manager_anchor {
        Some(Ok(a)) => a.epoch_ms,
        _ => worker_epoch,
    };
    let place = |epoch: i64, ms: u64| epoch.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX));

    let sizes = reply_sizes(ctl, &at, &hist, &mut sources);

    let mut turns: Vec<TurnRow> = hist
        .iter()
        .zip(sizes)
        .map(|(h, (rows, swallowed))| TurnRow {
            id: h.id,
            t: match &log_starts {
                Some(starts) => starts.get(&h.id).copied(),
                None => (!h.carried).then(|| place(worker_epoch, h.started_ms)),
            },
            status: h.status.clone(),
            submitted: h.submitted,
            dur_ms: h.dur_ms,
            carried: h.carried,
            began: log_starts.is_some(),
            text: h.text.clone(),
            rows,
            swallowed,
            stopped: None,
        })
        .collect();

    let mut journal = Vec::new();
    let mut journaled = false;
    let mut journal_unreadable = false;
    match &opts.journal {
        None => sources.push(Source {
            name: "journal",
            ok: false,
            detail: "none given: `--journal FILE` (the window's \
                     `<state>/drive/<sid>.journal.jsonl`) adds the watcher's lane"
                .to_string(),
        }),
        Some(path) => match read_journal(path) {
            Ok((records, bad)) => {
                journaled = true;
                let total = records.len();
                journal = records
                    .into_iter()
                    .filter(|r| r.sid.as_deref().is_none_or(|s| s == worker))
                    .collect();
                let other = total - journal.len();
                let mut detail =
                    format!("{}: {} line(s) about {at}", path.display(), journal.len());
                if other > 0 {
                    let _ = write!(detail, ", {other} about other sessions skipped");
                }
                if bad > 0 {
                    let _ = write!(detail, ", {bad} unreadable line(s) skipped");
                }
                sources.push(Source {
                    name: "journal",
                    ok: true,
                    detail,
                });
            }
            Err(e) => {
                journal_unreadable = true;
                sources.push(Source {
                    name: "journal",
                    ok: false,
                    detail: format!("unreadable: {e}"),
                });
            }
        },
    }

    let mut mail = Vec::new();
    let mut mail_notes = Vec::new();
    let mut mail_ok = false;
    match ask(ctl, &["@self", "inbox", "--peek", "--meta"]) {
        Ok(r) => {
            mail_ok = true;
            let (got, pending) = inbox_rows(&r.stdout, &opts.worker);
            mail_notes.push(format!("{} message(s) from {at} in the inbox", got.len()));
            if from_log {
                log_placed_mail(ctl, &worker, got, &pending, &mut mail, &mut mail_notes);
            } else {
                mail.extend(got.into_iter().map(|(_, t, m)| Mail {
                    t: Some(place(manager_epoch, t)),
                    ..m
                }));
                match ask(ctl, &["@self", "timeline"]) {
                    Ok(r) => {
                        let posts = timeline_posts(&r.stdout, &worker);
                        mail_notes.push(format!("{} post(s) to it in the timeline", posts.len()));
                        mail.extend(posts.into_iter().map(|(t, id, kind)| Mail {
                            t: Some(place(manager_epoch, t)),
                            dir: Dir::Out,
                            kind,
                            len: None,
                            trust: None,
                            pending: pending.contains(&id),
                        }));
                    }
                    Err(e) => mail_notes.push(format!("timeline: {e} (posts to it not shown)")),
                }
            }
        }
        Err(e) => mail_notes.push(format!("inbox: {e}")),
    }
    sources.push(Source {
        name: "mail",
        ok: mail_ok,
        detail: format!(
            "@self{}: {}",
            manager
                .as_deref()
                .map_or(String::new(), |m| format!(" ({m})")),
            mail_notes.join("; ")
        ),
    });

    if let Some(since) = opts.since_ms {
        turns.retain(|t| t.t.is_some_and(|t| t >= since));
        journal.retain(|r| r.t >= since);
        mail.retain(|m| m.t.is_some_and(|t| t >= since));
    }
    mail.sort_by_key(|m| m.t);

    let starts: Vec<Option<i64>> = turns.iter().map(|t| t.t).collect();
    for (i, turn) in turns.iter_mut().enumerate().filter(|_| aligned) {
        let Some(start) = turn.t else { continue };
        let next = starts[i + 1..].iter().flatten().next().copied();
        turn.stopped = journal
            .iter()
            .filter(|r| r.kind == "event" && ended_on_words(&r.phase))
            .map(|r| r.t)
            .find(|&t| t >= start && next.is_none_or(|n| t < n));
    }

    Ledger {
        worker,
        manager,
        now_ms: host.now_ms,
        tz_offset_s: host.tz_offset_s,
        since_ms: opts.since_ms,
        sources,
        turns,
        journal,
        journaled,
        journal_unreadable,
        mail,
        aligned,
        guessed,
    }
}

/// `--source log`'s mail (plan 5F-4): the inbox's messages from the worker
/// (`got`: id, process-clock `t`, row), each placed by its `mail dir=in`
/// mark's wall time joined by `inbox=` — one the log no longer holds keeps
/// no time, counted — and this session's posts to the worker from its `mail
/// dir=out` marks, `pending` (the inbox's own word) those not landed.
fn log_placed_mail<C: Ctl>(
    ctl: &mut C,
    worker: &str,
    got: Vec<(u64, u64, Mail)>,
    pending: &[u64],
    mail: &mut Vec<Mail>,
    notes: &mut Vec<String>,
) {
    let (ins, outs) = match ask(ctl, &["@self", "log", "kinds=mail"]) {
        Ok(r) => log_mail(&r.stdout, worker),
        Err(e) => {
            notes.push(format!(
                "log: {e} (no message placed, posts to it not shown)"
            ));
            (BTreeMap::new(), Vec::new())
        }
    };
    let unplaced = got.iter().filter(|(id, ..)| !ins.contains_key(id)).count();
    if unplaced > 0 {
        notes.push(format!("{unplaced} the log no longer holds: no time"));
    }
    mail.extend(got.into_iter().map(|(id, _, m)| Mail {
        t: ins.get(&id).copied(),
        ..m
    }));
    notes.push(format!("{} post(s) to it in the log", outs.len()));
    mail.extend(outs.into_iter().map(|(t, id, kind)| Mail {
        t: Some(t),
        dir: Dir::Out,
        kind,
        len: None,
        trust: None,
        pending: pending.contains(&id),
    }));
}

/// An EVENT the worker's turn ended with, waiting for words: `idle`, and in a journal an older
/// build's removed `watch --mail` wrote, its `turn` (a report folded in) and `idle-no-report`.
fn ended_on_words(phase: &str) -> bool {
    matches!(
        phase,
        "idle" | "question" | "limited" | "turn" | "idle-no-report"
    )
}

/// A turn's reply in rows (or why it is not known), and the turns inside that count.
type ReplySize = (Result<usize, String>, Vec<u64>);

fn reply_sizes<C: Ctl>(
    ctl: &mut C,
    at: &str,
    hist: &[HistTurn],
    sources: &mut Vec<Source>,
) -> Vec<ReplySize> {
    let unknown = |why: &str| vec![(Err(why.to_string()), Vec::new()); hist.len()];
    if hist.is_empty() {
        return Vec::new();
    }
    let max = format!("max={LEDGER_MAX_ROWS}");
    let tail = format!("tail={LEDGER_MAX_ROWS}");
    let r = match ask(ctl, &[at, "offscreen", &tail, &max, "screen=1"]) {
        Ok(r) => r,
        Err(e) => {
            sources.push(Source {
                name: "offscreen",
                ok: false,
                detail: format!("{at}: {e}: reply sizes not known"),
            });
            return unknown("no archive read");
        }
    };
    let off = match parse_offscreen(&r) {
        Ok(off) => off,
        Err(OffscreenError::NoHeader) => {
            sources.push(Source {
                name: "offscreen",
                ok: false,
                detail: "the reply carried no header (an `aterm ctl` too old to relay it)"
                    .to_string(),
            });
            return unknown("no archive read");
        }
        Err(OffscreenError::Bad(why)) => {
            sources.push(Source {
                name: "offscreen",
                ok: false,
                detail: format!("unreadable: {why}"),
            });
            return unknown("no archive read");
        }
    };
    let joined = join(&off, &aterm_phase::ClaudeReader);
    let rows = &joined.rows;
    let mut from = 0usize;
    let mut found: Vec<Option<usize>> = Vec::with_capacity(hist.len());
    for t in hist {
        let other_archive = t
            .arch
            .and_then(|m| m.origin)
            .is_some_and(|o| o != off.origin);
        let at_row = if other_archive {
            None
        } else {
            find_turn_row(rows, from, &t.text)
        };
        if let Some(a) = at_row {
            from = a + 1;
        }
        found.push(at_row);
    }
    let sizes: Vec<ReplySize> = (0..hist.len())
        .map(|i| {
            let Some(a) = found[i] else {
                let t = &hist[i];
                let why = if t
                    .arch
                    .and_then(|m| m.origin)
                    .is_some_and(|o| o != off.origin)
                {
                    "another archive (aterm restarted since)"
                } else if t
                    .arch
                    .is_some_and(|m| m.index.saturating_add(1) < joined.first)
                    && joined.archived > 0
                {
                    "older than the rows the archive holds"
                } else {
                    "its ❯ row was not found"
                };
                return (Err(why.to_string()), Vec::new());
            };
            let next = found[i + 1..].iter().position(Option::is_some);
            let end = match next {
                Some(k) => found[i + 1 + k].unwrap_or(rows.len()),
                None => rows.len(),
            };
            let swallowed = hist[i + 1..][..next.unwrap_or(hist.len() - i - 1)]
                .iter()
                .map(|t| t.id)
                .collect();
            let body = &rows[a..end];
            (
                Ok(body
                    .iter()
                    .rposition(|r| !r.trim().is_empty())
                    .map_or(0, |e| e + 1)),
                swallowed,
            )
        })
        .collect();
    let known = sizes.iter().filter(|(s, _)| s.is_ok()).count();
    sources.push(Source {
        name: "offscreen",
        ok: true,
        detail: format!(
            "{} row(s) read from {at}'s archive (origin {}){}: reply sizes for {known} of {} turn(s)",
            rows.len(),
            off.origin,
            if off.more { ", the oldest left out" } else { "" },
            hist.len()
        ),
    });
    sizes
}

/// The first user row at or after `from` whose text begins with `text`'s first 60 characters
/// (whitespace aside), else, for a paste-like text, the first `[Pasted text …]` row that fits.
fn find_turn_row(rows: &[String], from: usize, text: &str) -> Option<usize> {
    let want: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(MARKER_CHARS)
        .collect();
    let need = want.chars().count();
    let users = (from..rows.len()).filter(|&i| is_user_row(&rows[i]));
    if need > 0
        && let Some(i) = users.clone().find(|&i| user_text(rows, i, need) == want)
    {
        return Some(i);
    }
    if !paste_like(text) {
        return None;
    }
    let mut users = users;
    users.find(|&i| {
        let body = rows[i].trim_start_matches('❯').trim_start();
        body.starts_with(PASTED) && paste_fits(body, text)
    })
}

/// Aggregate turn, working-time, mail, and latency statistics for a [`Ledger`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    pub turns: usize,
    pub settled: usize,
    pub timeouts: usize,
    pub unsubmitted: usize,
    pub carried: usize,
    /// The sum of the turns' `dur_ms` (type → submit → settle).
    pub turn_ms: u64,
    /// From each turn's start to the journal's EVENT that ended it, summed,
    /// and over how many turns.
    pub working_ms: i64,
    pub working_turns: usize,
    /// From each EVENT idle or question to the next turn's start.
    pub latencies: Vec<i64>,
    /// EVENT idle or question with no turn after it.
    pub waiting: usize,
    pub approvals: usize,
    pub dismissals: usize,
    pub context: usize,
    pub compacted: usize,
    pub reconnects: usize,
    pub timeouts_seen: usize,
    pub exits: usize,
    pub mail_in: usize,
    pub mail_out: usize,
}

impl Summary {
    pub fn median_ms(&self) -> Option<i64> {
        let mut v = self.latencies.clone();
        v.sort_unstable();
        let n = v.len();
        match n {
            0 => None,
            _ if n % 2 == 1 => Some(v[n / 2]),
            _ => Some((v[n / 2 - 1] + v[n / 2]) / 2),
        }
    }
    pub fn max_ms(&self) -> Option<i64> {
        self.latencies.iter().copied().max()
    }
}

/// The next turn's start after `t`.
fn next_turn(turns: &[TurnRow], t: i64) -> Option<i64> {
    turns.iter().filter_map(|x| x.t).filter(|&s| s > t).min()
}

impl Ledger {
    /// Compute aggregate summary statistics for this ledger.
    pub fn summary(&self) -> Summary {
        let mut s = Summary {
            turns: self.turns.len(),
            ..Summary::default()
        };
        for t in &self.turns {
            match t.status.as_str() {
                "settled" => s.settled += 1,
                "timeout" => s.timeouts += 1,
                _ => {}
            }
            s.unsubmitted += usize::from(!t.submitted);
            s.carried += usize::from(t.carried);
            s.turn_ms += t.dur_ms;
            if let (Some(a), Some(b)) = (t.t, t.stopped) {
                s.working_ms += b - a;
                s.working_turns += 1;
            }
        }
        for r in &self.journal {
            match (r.kind.as_str(), r.phase.as_str()) {
                ("event", "idle" | "question" | "turn" | "idle-no-report") => {
                    match next_turn(&self.turns, r.t).filter(|_| self.aligned) {
                        Some(start) => s.latencies.push(start - r.t),
                        None if self.aligned => s.waiting += 1,
                        None => {}
                    }
                }
                ("event", "context") => s.context += 1,
                ("event", "compacted") => s.compacted += 1,
                ("approved", _) => s.approvals += 1,
                ("dismissed", _) => s.dismissals += 1,
                ("reconnect", _) if r.line.starts_with("RECONNECT ") => s.reconnects += 1,
                ("timeout", _) => s.timeouts_seen += 1,
                ("exit", _) => s.exits += 1,
                _ => {}
            }
        }
        for m in &self.mail {
            match m.dir {
                Dir::In => s.mail_in += 1,
                Dir::Out => s.mail_out += 1,
            }
        }
        s
    }

    /// Build the time-ordered timeline items for this ledger.
    pub fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for t in &self.turns {
            let sent = if t.began { "began" } else { "sent" };
            let settle = if !t.submitted {
                format!("not submitted ({})", ms(t.dur_ms))
            } else if t.status == "settled" {
                format!("settled in {}", ms(t.dur_ms))
            } else {
                format!("{} after {}", t.status, ms(t.dur_ms))
            };
            items.push(Item {
                t: t.t,
                lane: Lane::Manager,
                mark: MarkKind::Turn,
                what: format!("turn {} {sent}: {}", t.id, cut(&t.text, TURN_CHARS)),
                full: format!(
                    "turn {} {sent}{}: {}",
                    t.id,
                    if t.carried { " (carried)" } else { "" },
                    t.text
                ),
                dur: settle,
                end: None,
                long_end: None,
            });
            let also = if t.swallowed.is_empty() {
                String::new()
            } else {
                let ids: Vec<String> = t.swallowed.iter().map(u64::to_string).collect();
                format!(
                    " (turn {}'s too: its ❯ row was not found)",
                    ids.join(" and ")
                )
            };
            let (what, full) = match &t.rows {
                Ok(n) => (
                    format!("turn {} reply: {n} rows{also}", t.id),
                    format!(
                        "turn {} — the worker's reply is {n} rows in the archive{also}",
                        t.id
                    ),
                ),
                Err(why) => (
                    format!("turn {} reply: size not known ({why})", t.id),
                    format!("turn {} — reply size not known: {why}", t.id),
                ),
            };
            items.push(Item {
                t: t.t,
                lane: Lane::Worker,
                mark: MarkKind::Reply,
                what,
                full,
                dur: match (t.t, t.stopped) {
                    (Some(a), Some(b)) => format!("working {}", span(b - a)),
                    _ => "-".to_string(),
                },
                end: t.t.map(|a| a + i64::try_from(t.dur_ms).unwrap_or(0)),
                long_end: t.stopped,
            });
        }
        for r in &self.journal {
            let dur = if r.kind == "event"
                && matches!(
                    r.phase.as_str(),
                    "idle" | "question" | "turn" | "idle-no-report"
                ) {
                match next_turn(&self.turns, r.t).filter(|_| self.aligned) {
                    Some(start) => format!("answered in {}", span(start - r.t)),
                    None if self.aligned => "no turn since".to_string(),
                    None => "-".to_string(),
                }
            } else {
                "-".to_string()
            };
            items.push(Item {
                t: Some(r.t),
                lane: Lane::Watcher,
                mark: MarkKind::of_record(r),
                what: r.line.clone(),
                full: r.line.clone(),
                dur,
                end: None,
                long_end: None,
            });
        }
        for m in &self.mail {
            let (what, lane_word) = match m.dir {
                Dir::In => (format!("mail in: {} from the worker", m.kind), "in"),
                Dir::Out => (format!("mail out: {} to the worker", m.kind), "out"),
            };
            let mut detail = what.clone();
            if let Some(n) = m.len {
                let _ = write!(detail, ", {n} B");
            }
            if let Some(trust) = &m.trust {
                let _ = write!(detail, ", trust={trust}");
            }
            if m.pending {
                detail.push_str(", not landed yet");
            }
            items.push(Item {
                t: m.t,
                lane: Lane::Fabric,
                mark: if lane_word == "in" {
                    MarkKind::MailIn
                } else {
                    MarkKind::MailOut
                },
                what: detail.clone(),
                full: detail,
                dur: "-".to_string(),
                end: None,
                long_end: None,
            });
        }
        items.sort_by_key(|i| (i.t, i.lane as u8));
        items
    }
}

/// Swimlane classification for a timeline item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    Manager = 0,
    Worker = 1,
    Watcher = 2,
    Fabric = 3,
}

impl Lane {
    pub const fn name(self) -> &'static str {
        match self {
            Lane::Manager => "manager",
            Lane::Worker => "worker",
            Lane::Watcher => "watcher",
            Lane::Fabric => "fabric",
        }
    }
}

/// What a timeline item marks, for the HTML page's marks and legend: the manager's turns
/// and the worker's replies, the watcher's lines (an `EVENT` by its phase, every other
/// journal [`Word`] and [`Noted`] word by its own kind), the fabric's mail, and `unread` for a
/// line this reader does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Turn,
    Reply,
    Idle,
    Question,
    Prompt,
    Limited,
    Survey,
    Context,
    Busy,
    Approved,
    Dismissed,
    Reconnect,
    Timeout,
    Exit,
    Mail,
    Extend,
    Escalated,
    Cleared,
    Continued,
    Typed,
    Restarted,
    Waiting,
    Skipped,
    Unverified,
    LimitHandled,
    Unproven,
    Dialog,
    Declined,
    Chose,
    Exception,
    Probed,
    Reset,
    Needs,
    Hold,
    Occupancy,
    Probe,
    /// A line of a word the journal keeps as `other` ([`Noted`]).
    Noted(Noted),
    MailIn,
    MailOut,
    Unread,
}

impl MarkKind {
    /// The marks before the [`Noted`] words', in the legend's order.
    const READ: [Self; 36] = [
        Self::Turn,
        Self::Reply,
        Self::Idle,
        Self::Question,
        Self::Prompt,
        Self::Limited,
        Self::Survey,
        Self::Context,
        Self::Busy,
        Self::Approved,
        Self::Dismissed,
        Self::Reconnect,
        Self::Timeout,
        Self::Exit,
        Self::Mail,
        Self::Extend,
        Self::Escalated,
        Self::Cleared,
        Self::Continued,
        Self::Typed,
        Self::Restarted,
        Self::Waiting,
        Self::Skipped,
        Self::Unverified,
        Self::LimitHandled,
        Self::Unproven,
        Self::Dialog,
        Self::Declined,
        Self::Chose,
        Self::Exception,
        Self::Probed,
        Self::Reset,
        Self::Needs,
        Self::Hold,
        Self::Occupancy,
        Self::Probe,
    ];

    /// The marks after them.
    const AFTER: [Self; 3] = [Self::MailIn, Self::MailOut, Self::Unread];

    /// Every mark, in the legend's order: the read words', each [`Noted`] word's in
    /// [`Noted::ALL`]'s order, then the mail's and `unread`.
    pub const ALL: [Self; Self::READ.len() + Noted::ALL.len() + Self::AFTER.len()] = {
        let mut all = [Self::Unread; Self::READ.len() + Noted::ALL.len() + Self::AFTER.len()];
        let mut i = 0;
        while i < Self::READ.len() {
            all[i] = Self::READ[i];
            i += 1;
        }
        let mut j = 0;
        while j < Noted::ALL.len() {
            all[i + j] = Self::Noted(Noted::ALL[j]);
            j += 1;
        }
        let mut k = 0;
        while k < Self::AFTER.len() {
            all[i + j + k] = Self::AFTER[k];
            k += 1;
        }
        all
    };

    /// The mark of a journal record: its word's own, an `EVENT` by its phase, a line kept as
    /// `other` by its first word (a [`Noted`] word, or a [`Word`] an older build did not read
    /// yet), `unread` for a kind, word or `EVENT` phase this reader does not know.
    #[must_use]
    pub fn of_record(r: &JournalRecord) -> Self {
        if let Some(word) = Word::of_kind(&r.kind) {
            return Self::of_word(word, &r.phase);
        }
        if r.kind != "other" {
            return Self::Unread;
        }
        if let Some(w) = Noted::of_line(&r.line) {
            return Self::Noted(w);
        }
        let now = JournalRecord::of_line(r.t, None, &r.line, None);
        Word::of_kind(&now.kind).map_or(Self::Unread, |word| Self::of_word(word, &now.phase))
    }

    /// The mark of a line read as `word`, about `phase`.
    fn of_word(word: Word, phase: &str) -> Self {
        match word {
            Word::Event => match phase {
                "idle" | "turn" | "idle-no-report" | "resumed" => Self::Idle,
                "question" => Self::Question,
                "prompt" => Self::Prompt,
                "limited" | "still-limited" => Self::Limited,
                "survey" => Self::Survey,
                "context" | "compacted" => Self::Context,
                "busy" => Self::Busy,
                _ => Self::Unread,
            },
            Word::Approved => Self::Approved,
            Word::Dismissed => Self::Dismissed,
            Word::Reconnect => Self::Reconnect,
            Word::Timeout => Self::Timeout,
            Word::Exit => Self::Exit,
            Word::Mail => Self::Mail,
            Word::Extend => Self::Extend,
            Word::Escalated => Self::Escalated,
            Word::Cleared => Self::Cleared,
            Word::Continued => Self::Continued,
            Word::Typed => Self::Typed,
            Word::Restarted => Self::Restarted,
            Word::Waiting => Self::Waiting,
            Word::Skipped => Self::Skipped,
            Word::Unverified => Self::Unverified,
            Word::Limited => Self::LimitHandled,
            Word::Unproven => Self::Unproven,
            Word::Dialog => Self::Dialog,
            Word::Declined => Self::Declined,
            Word::Chose => Self::Chose,
            Word::Exception => Self::Exception,
            Word::Probed => Self::Probed,
            Word::Reset => Self::Reset,
            Word::Needs => Self::Needs,
            Word::Hold => Self::Hold,
            Word::Occupancy => Self::Occupancy,
            Word::Probe => Self::Probe,
        }
    }

    /// The mark's word in the page's data and legend.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Reply => "reply",
            Self::Idle => "idle",
            Self::Question => "question",
            Self::Prompt => "prompt",
            Self::Limited => "limited",
            Self::Survey => "survey",
            Self::Context => "context",
            Self::Busy => "busy",
            Self::Approved => "approved",
            Self::Dismissed => "dismissed",
            Self::Reconnect => "reconnect",
            Self::Timeout => "timeout",
            Self::Exit => "exit",
            Self::Mail => "mail",
            Self::Extend => "extend",
            Self::Escalated => "escalated",
            Self::Cleared => "cleared",
            Self::Continued => "continued",
            Self::Typed => "typed",
            Self::Restarted => "restarted",
            Self::Waiting => "waiting",
            Self::Skipped => "skipped",
            Self::Unverified => "unverified",
            Self::LimitHandled => "limit-handled",
            Self::Unproven => "unproven",
            Self::Dialog => "dialog",
            Self::Declined => "declined",
            Self::Chose => "chose",
            Self::Exception => "exception",
            Self::Probed => "probed",
            Self::Reset => "reset",
            Self::Needs => "needs",
            Self::Hold => "hold",
            Self::Occupancy => "occupancy",
            Self::Probe => "probe",
            Self::Noted(w) => w.name(),
            Self::MailIn => "mail-in",
            Self::MailOut => "mail-out",
            Self::Unread => "unread",
        }
    }
}

/// A test build's check of each line a loop journals ([`super::journal::Journal::record`]):
/// the ledger marks it by a word of its own, never `unread`.
#[cfg(test)]
pub(crate) fn assert_marked(line: &str) {
    let mark = MarkKind::of_record(&JournalRecord::of_line(0, None, line, None));
    assert!(
        mark != MarkKind::Unread,
        "a loop journaled `{line}`, which the ledger reads `unread`: give its word a \
         journal::Noted (or Word) entry, a legend text (ledger_html::marks) and a swatch"
    );
}

/// Single formatted timeline row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub t: Option<i64>,
    pub lane: Lane,
    pub mark: MarkKind,
    /// The WHAT column (cut for a terminal).
    pub what: String,
    /// The whole of it (the HTML's hover).
    pub full: String,
    /// The duration/latency column.
    pub dur: String,
    /// A turn's bar: where the `turn` verb settled.
    pub end: Option<i64>,
    /// A turn's bar: where the worker stopped (the journal's EVENT).
    pub long_end: Option<i64>,
}

/// `1.8s`, `950ms`.
fn ms(n: u64) -> String {
    if n < 1000 {
        format!("{n}ms")
    } else {
        format!("{}.{}s", n / 1000, (n % 1000) / 100)
    }
}

/// Format a duration in milliseconds as `42s`, `3m10s`, or `2h05m`.
pub fn span(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Format a Unix millisecond timestamp in local time (`HH:MM:SS` or `YYYY-MM-DD HH:MM:SS`).
pub fn stamp(ms: i64, offset_s: i64, date: bool) -> String {
    let secs = ms.div_euclid(1000) + offset_s;
    let (y, mo, d) = civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let hms = format!("{:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60);
    if date {
        format!("{y:04}-{mo:02}-{d:02} {hms}")
    } else {
        hms
    }
}

/// Format a UTC offset in seconds as `UTC±HH:MM`.
pub fn zone(offset_s: i64) -> String {
    let sign = if offset_s < 0 { '-' } else { '+' };
    let a = offset_s.abs();
    format!("UTC{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60)
}

/// Parse a `--since` argument (Unix ms, `YYYY-MM-DD`, or ISO-8601 timestamp).
pub fn parse_since(s: &str, local_offset_s: i64) -> Option<i64> {
    let s = s.trim();
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    let num = |t: &str| -> Option<u32> {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse().ok())
            .flatten()
    };
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut parts = date.split('-');
    let y = i64::from(num(parts.next()?)?);
    let mo = num(parts.next()?)?;
    let d = num(parts.next()?)?;
    if parts.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (mut offset, mut secs) = (local_offset_s, 0i64);
    if let Some(time) = time {
        let (clock, zone) = if let Some(c) = time.strip_suffix('Z') {
            (c, Some(0))
        } else if let Some(at) = time.rfind(['+', '-']) {
            let (c, z) = time.split_at(at);
            let (sign, z) = z.split_at(1);
            let (zh, zm) = match z.split_once(':') {
                Some(pair) => pair,
                None if z.len() == 4 && z.is_char_boundary(2) => z.split_at(2),
                None => (z, "0"),
            };
            let v = i64::from(num(zh)?) * 3600 + i64::from(num(zm)?) * 60;
            (c, Some(if sign == "-" { -v } else { v }))
        } else {
            (time, None)
        };
        let mut hms = clock.split(':');
        let h = num(hms.next()?)?;
        let m = num(hms.next()?)?;
        let sec = hms.next().map_or(Some(0), num)?;
        if hms.next().is_some() || h > 23 || m > 59 || sec > 60 {
            return None;
        }
        secs = i64::from(h) * 3600 + i64::from(m) * 60 + i64::from(sec);
        if let Some(z) = zone {
            offset = z;
        }
    }
    days_from_civil(y, mo, d)
        .checked_mul(86_400)?
        .checked_add(secs)?
        .checked_sub(offset)?
        .checked_mul(1000)
}

/// The time column: `HH:MM:SS`, with the date when the rows span more than
/// one local day; `-` for none; `~` before a time the clock could only guess
/// ([`Ledger::guessed`]).
fn time_cell(t: Option<i64>, l: &Ledger, dated: bool) -> String {
    match t {
        None => "-".to_string(),
        Some(t) => {
            let s = stamp(t, l.tz_offset_s, dated);
            if l.guessed { format!("~{s}") } else { s }
        }
    }
}

fn spans_days(items: &[Item], offset_s: i64) -> bool {
    let days: Vec<i64> = items
        .iter()
        .filter_map(|i| i.t)
        .map(|t| (t.div_euclid(1000) + offset_s).div_euclid(86_400))
        .collect();
    days.iter().min() != days.iter().max()
}

/// Format [`Summary`] metrics as `(label, value)` rows.
pub fn summary_rows(l: &Ledger, s: &Summary) -> Vec<(&'static str, String)> {
    let nj = if l.journal_unreadable {
        "- (the journal could not be read)".to_string()
    } else {
        "- (no journal)".to_string()
    };
    let mut turns = format!("{} ({} settled, {} timeout", s.turns, s.settled, s.timeouts);
    if s.unsubmitted > 0 {
        let _ = write!(turns, ", {} not submitted", s.unsubmitted);
    }
    if s.carried > 0 {
        let _ = write!(turns, ", {} carried", s.carried);
    }
    turns.push(')');
    let mut busy = format!("{} in the turn verbs (sum of dur_ms)", ms(s.turn_ms));
    if s.working_turns > 0 {
        let _ = write!(
            busy,
            " · {} working over {} turn(s) (turn start → the EVENT that ended it)",
            span(s.working_ms),
            s.working_turns
        );
    }
    let latency = if !l.journaled {
        nj.clone()
    } else if l.guessed {
        "- (the clock could not be placed)".to_string()
    } else if !l.aligned {
        "- (the session log could not be read)".to_string()
    } else {
        match (s.median_ms(), s.max_ms()) {
            (Some(med), Some(max)) => format!(
                "median {} · max {} over {} EVENT idle/question{}",
                span(med),
                span(max),
                s.latencies.len(),
                if s.waiting > 0 {
                    format!(" · {} still waiting", s.waiting)
                } else {
                    String::new()
                }
            ),
            _ if s.waiting > 0 => format!("- ({} EVENT idle/question still waiting)", s.waiting),
            _ => "- (no EVENT idle/question in the journal)".to_string(),
        }
    };
    let j = |v: String| if l.journaled { v } else { nj.clone() };
    vec![
        ("turns driven", turns),
        ("worker busy", busy),
        ("manager latency", latency),
        ("approvals", j(s.approvals.to_string())),
        ("dismissals", j(s.dismissals.to_string())),
        (
            "context",
            j(format!("{} context · {} compacted", s.context, s.compacted)),
        ),
        ("reconnects", j(s.reconnects.to_string())),
        ("mail", format!("{} in · {} out", s.mail_in, s.mail_out)),
    ]
}

fn heading(l: &Ledger) -> String {
    format!(
        "worker @{} · manager {} · {} · times {}{}",
        l.worker,
        l.manager
            .as_deref()
            .map_or("-".to_string(), |m| format!("@{m}")),
        stamp(l.now_ms, l.tz_offset_s, true),
        zone(l.tz_offset_s),
        l.since_ms.map_or(String::new(), |s| format!(
            " · since {}",
            stamp(s, l.tz_offset_s, true)
        ))
    )
}

/// Pad to `w` characters.
fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

/// Render the ledger as aligned plain-text tables.
pub fn render_text(l: &Ledger) -> String {
    let s = l.summary();
    let items = l.items();
    let dated = spans_days(&items, l.tz_offset_s);
    let mut out = format!("aterm drive ledger — {}\n", heading(l));
    out.push_str("SOURCES\n");
    for src in &l.sources {
        let _ = writeln!(
            out,
            "  {} {} {}",
            if src.ok { "✓" } else { "✗" },
            pad(src.name, 10),
            one_line(&src.detail)
        );
    }
    out.push_str("SUMMARY\n");
    for (k, v) in summary_rows(l, &s) {
        let _ = writeln!(out, "  {}  {v}", pad(k, 16));
    }
    out.push_str("TIMELINE\n");
    if items.is_empty() {
        out.push_str("  nothing to show\n");
        return out;
    }
    let tw = if dated { 20 } else { 9 } + usize::from(l.guessed);
    let _ = writeln!(
        out,
        "  {}  {}  {}  DURATION/LATENCY",
        pad("TIME", tw),
        pad("LANE", 7),
        pad("WHAT", WHAT_CHARS)
    );
    for i in &items {
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}",
            pad(&time_cell(i.t, l, dated), tw),
            pad(i.lane.name(), 7),
            pad(&cut(&i.what, WHAT_CHARS), WHAT_CHARS),
            i.dur
        );
    }
    out
}

/// A markdown table cell: one line, `|` escaped.
fn md_cell(s: &str) -> String {
    one_line(s).replace('\\', "\\\\").replace('|', "\\|")
}

/// Render the ledger as Markdown tables.
pub fn render_md(l: &Ledger) -> String {
    let s = l.summary();
    let items = l.items();
    let dated = spans_days(&items, l.tz_offset_s);
    let mut out = format!("# aterm drive ledger\n\n{}\n\n", md_cell(&heading(l)));
    out.push_str("## Summary\n\n| | |\n|---|---|\n");
    for (k, v) in summary_rows(l, &s) {
        let _ = writeln!(out, "| {k} | {} |", md_cell(&v));
    }
    out.push_str("\n## Sources\n\n| source | read | detail |\n|---|---|---|\n");
    for src in &l.sources {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            src.name,
            if src.ok { "yes" } else { "no" },
            md_cell(&src.detail)
        );
    }
    out.push_str("\n## Timeline\n\n");
    if items.is_empty() {
        out.push_str("Nothing to show.\n");
        return out;
    }
    out.push_str("| time | lane | what | duration/latency |\n|---|---|---|---|\n");
    for i in &items {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            time_cell(i.t, l, dated),
            i.lane.name(),
            md_cell(&i.what),
            md_cell(&i.dur)
        );
    }
    out
}

/// Encode `s` as an HTML-safe JSON string literal.
pub(super) fn html_json_str(s: &str) -> String {
    json_str(s)
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// Escape `&`, `<`, `>`, `"`, and `'` for HTML text content.
pub(super) fn html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Render `l` in the requested [`Format`].
pub fn render(l: &Ledger, format: Format) -> String {
    match format {
        Format::Text => render_text(l),
        Format::Md => render_md(l),
        Format::Html => super::ledger_html::render_html(l),
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
