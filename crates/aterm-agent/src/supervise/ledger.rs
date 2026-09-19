// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive ledger`: how the manager loop ran, replayed — the owner asked
//! for "some kind of way to visualize how you are seeing messages and
//! managing". One timeline joins four sources:
//!
//! 1. the WORKER's turn ledger (`history`): every turn the manager typed,
//!    when it started, how long the `turn` verb took, how it settled;
//! 2. the watcher's JOURNAL (`watch --journal`, [`super::journal`]), when
//!    given: every EVENT, APPROVED, DISMISSED, RECONNECT, TIMEOUT and EXIT
//!    line, with the Unix time it was printed;
//! 3. the MANAGER's fabric mail with that worker: this session's `inbox
//!    --peek --meta` rows from it and the `post` rows of this session's
//!    `timeline` addressed to it (read without listing or handling anything);
//! 4. for each turn, the size of the worker's reply in rows, from ONE
//!    `offscreen tail=… screen=1` read joined the way `report` joins it: the
//!    rows from the turn's `❯` row to the next turn's.
//!
//! The ledger, the inbox and the timeline are stamped by the aterm process's
//! own clock (milliseconds since it started its clock, not wall time); the
//! journal by the wall clock. They are put on one axis by where each process
//! started its clock, which the HOST says ([`LedgerHost::anchor`]; the CLI
//! uses the birth time of the instance's control socket — measured on
//! 2026-09-14 within 0.12 s of the fabric bus's own wall-clock stamps on the
//! same messages, where the process's start time was 5.3 s early). A turn
//! carried from an earlier process by a self-update (`carried=1`) is on that
//! process's clock, and has no time here.
//!
//! A source that cannot be read is named in SOURCES and the rest is printed.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::journal::{JournalRecord, json_str, read_journal};
use super::limit::{civil, days_from_civil};
use super::mail::{bare_sid, field};
use super::report::{
    MARKER_CHARS, Mark, OffscreenError, PASTED, decimal, is_user_row, join, parse_offscreen,
    paste_fits, paste_like, pct_decode, user_text, without_verb,
};
use super::run::{Ctl, CtlReply};

/// The most archived rows the reply-size read asks for (the archive is 4 MiB).
pub const LEDGER_MAX_ROWS: usize = 20_000;
/// How many characters of a turn's text the timeline shows.
const TURN_CHARS: usize = 100;
/// How wide the text timeline's WHAT column is.
const WHAT_CHARS: usize = 96;

/// `--format`.
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

/// What to put in the ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerOpts {
    /// The worker, as `@s-…`.
    pub worker: String,
    /// `--journal`: the watcher's journal.
    pub journal: Option<PathBuf>,
    /// `--since`: nothing before this Unix time (ms).
    pub since_ms: Option<i64>,
}

/// Where a session's aterm process started the clock its `history`,
/// `inbox` and `timeline` stamps count from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockAnchor {
    /// The instance's pid.
    pub pid: u32,
    /// The Unix time (ms) of its clock's zero.
    pub epoch_ms: i64,
    /// How that was found, for SOURCES.
    pub how: String,
}

/// What the ledger needs from the machine besides the control socket.
pub struct LedgerHost<'a> {
    /// The time now, Unix ms.
    pub now_ms: i64,
    /// The local time's offset from UTC, seconds (times print in it).
    pub tz_offset_s: i64,
    /// Where the process hosting a session (by sid, no `@`) started its
    /// clock.
    pub anchor: &'a mut dyn FnMut(&str) -> Result<ClockAnchor, String>,
}

/// One source, read or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// One `history` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistTurn {
    pub id: u64,
    pub submitted: bool,
    pub status: String,
    pub started_ms: u64,
    pub dur_ms: u64,
    pub arch: Option<Mark>,
    pub carried: bool,
    pub text: String,
}

/// The records of a `history` reply (`turn <id> submitted=… status=…
/// started_ms=… dur_ms=… … arch=<o>:<l> [carried=1] text=<pct>`); any other
/// line is skipped. `text=` is the free-text tail, so the line is cut there
/// first.
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

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// From the worker to this session.
    In,
    /// From this session to the worker.
    Out,
}

/// One message between the manager and the worker.
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

/// The `inbox --peek --meta` rows from `worker` (`msg … t=<ms> from=<sid@node>
/// kind=… trust=… len=…`) with their process-clock `t`, and the ids of the
/// posts to it that have not landed (`post <id> to=… off=-`).
fn inbox_rows(body: &str, worker: &str) -> (Vec<(u64, Mail)>, Vec<u64>) {
    let mut got = Vec::new();
    let mut pending = Vec::new();
    for row in body.lines() {
        if row.starts_with("msg ") {
            if field(row, "from").map(bare_sid) != Some(worker) {
                continue;
            }
            let t = field(row, "t").and_then(decimal);
            got.push((
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
            && field(row, "to").map(bare_sid) == Some(worker)
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

/// One turn, placed on the wall clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub id: u64,
    /// Aligned Unix ms of its start (`None`: carried, or no clock).
    pub t: Option<i64>,
    pub status: String,
    pub submitted: bool,
    pub dur_ms: u64,
    pub carried: bool,
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

/// Everything read, placed on one clock.
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
    /// Whether a `--journal` was GIVEN and could not be read. Not the same as
    /// none given: nothing of the watcher's loop was read, so the counts it
    /// would have filled say that instead of `0`.
    pub journal_unreadable: bool,
    pub mail: Vec<Mail>,
    /// Whether the process-clock stamps are on the wall clock.
    pub aligned: bool,
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

/// Read every source (see the module doc). Never fails: a source that
/// cannot be read is a [`Source`] with `ok: false`.
pub fn gather<C: Ctl>(ctl: &mut C, opts: &LedgerOpts, host: &mut LedgerHost<'_>) -> Ledger {
    let worker = bare_sid(&opts.worker).to_string();
    let at = format!("@{worker}");
    let mut sources = Vec::new();

    // Whose clock: the worker's instance, and this session's.
    let manager = ask(ctl, &["whoami"])
        .ok()
        .and_then(|r| r.stdout.split_whitespace().nth(1).map(str::to_string));
    let worker_anchor = (host.anchor)(&worker);
    let manager_anchor = manager.as_deref().map(|m| (host.anchor)(m));
    let aligned = worker_anchor.is_ok();
    sources.push(Source {
        name: "clock",
        ok: aligned,
        detail: match &worker_anchor {
            Ok(a) => format!(
                "instance {}'s clock started {} ({}); history, inbox and timeline times are \
                 placed by it",
                a.pid,
                stamp(a.epoch_ms, host.tz_offset_s, true),
                a.how
            ),
            Err(e) => format!(
                "not placed on the wall clock ({e}): worker and mail times are shown as if its \
                 newest record were now, and no latency is measured"
            ),
        },
    });

    // 1. The turn ledger.
    let mut hist: Vec<HistTurn> = Vec::new();
    match ask(ctl, &[&at, "history"]) {
        Ok(r) => {
            hist = parse_history_records(&r.stdout);
            let carried = hist.iter().filter(|t| t.carried).count();
            sources.push(Source {
                name: "history",
                ok: !hist.is_empty(),
                detail: if hist.is_empty() {
                    format!("{at} has no turns in its ledger (typed by hand or by `drive prompt`?)")
                } else {
                    format!(
                        "{} turn(s) from {at}{}",
                        hist.len(),
                        if carried > 0 {
                            format!(", {carried} carried from an earlier aterm (no time)")
                        } else {
                            String::new()
                        }
                    )
                },
            });
        }
        Err(e) => sources.push(Source {
            name: "history",
            ok: false,
            detail: format!("{at}: {e}"),
        }),
    }

    // The process-clock → wall-clock step. Without an anchor the newest
    // stamp read is taken as now, so the order still reads.
    let newest = hist
        .iter()
        .filter(|t| !t.carried)
        .map(|t| t.started_ms)
        .max();
    let fallback = host.now_ms - i64::try_from(newest.unwrap_or(0)).unwrap_or(0);
    let worker_epoch = worker_anchor.as_ref().map_or(fallback, |a| a.epoch_ms);
    let manager_epoch = match &manager_anchor {
        Some(Ok(a)) => a.epoch_ms,
        _ => worker_epoch,
    };
    let place = |epoch: i64, ms: u64| epoch.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX));

    // 4. The reply sizes.
    let sizes = reply_sizes(ctl, &at, &hist, &mut sources);

    let mut turns: Vec<TurnRow> = hist
        .iter()
        .zip(sizes)
        .map(|(h, (rows, swallowed))| TurnRow {
            id: h.id,
            t: (!h.carried).then(|| place(worker_epoch, h.started_ms)),
            status: h.status.clone(),
            submitted: h.submitted,
            dur_ms: h.dur_ms,
            carried: h.carried,
            text: h.text.clone(),
            rows,
            swallowed,
            stopped: None,
        })
        .collect();

    // 2. The journal.
    //
    // `journaled` is whether one was READ, not whether one was GIVEN: a
    // `--journal` that does not open has no records to say anything about, and
    // counting it as zero approvals, zero reconnects and "no EVENT
    // idle/question in the journal" reports the watcher's loop as idle when
    // nothing about it was read at all.
    let mut journal = Vec::new();
    let mut journaled = false;
    let mut journal_unreadable = false;
    match &opts.journal {
        None => sources.push(Source {
            name: "journal",
            ok: false,
            detail: "none given: `--journal FILE` (the file `watch --journal` wrote) adds the \
                     watcher's lane"
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

    // 3. The mail.
    let mut mail = Vec::new();
    let mut mail_notes = Vec::new();
    let mut mail_ok = false;
    match ask(ctl, &["@self", "inbox", "--peek", "--meta"]) {
        Ok(r) => {
            mail_ok = true;
            let (got, pending) = inbox_rows(&r.stdout, &worker);
            mail_notes.push(format!("{} message(s) from {at} in the inbox", got.len()));
            mail.extend(got.into_iter().map(|(t, m)| Mail {
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

    // --since.
    if let Some(since) = opts.since_ms {
        turns.retain(|t| t.t.is_some_and(|t| t >= since));
        journal.retain(|r| r.t >= since);
        mail.retain(|m| m.t.is_some_and(|t| t >= since));
    }
    mail.sort_by_key(|m| m.t);

    // When the worker stopped: the first EVENT that ended on words after
    // each turn began, before the next one did.
    //
    // ONLY WITH A CLOCK. `turn.t` is a PROCESS-clock stamp placed on an epoch;
    // without an anchor that epoch is the `fallback` above (the newest turn
    // read is taken as now, so the ORDER still reads), while a journal record's
    // `t` is a wall clock the watcher wrote. `stopped - t` across those two is
    // not a duration, and the summary's `working` span, the TIMELINE's
    // `working <span>` and the HTML's long bar would all print it as one —
    // while `manager latency` on the same render says the clock could not be
    // placed. What cannot be placed claims no latency, here too.
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
    }
}

/// An EVENT the worker's turn ended with: it stopped and waits for words —
/// `idle` as `watch --mail` prints it too (`turn`, its report folded in, and
/// `idle-no-report`).
fn ended_on_words(phase: &str) -> bool {
    matches!(
        phase,
        "idle" | "question" | "limited" | "turn" | "idle-no-report"
    )
}

/// The worker's reply to each turn, in rows: ONE `offscreen tail=<n> max=<n>
/// screen=1` read, joined as `report` joins it; each turn's `❯` row found by
/// its text (the first 60 characters, whitespace aside, or a `[Pasted text …]`
/// row that fits), in order; a reply is its rows down to the next turn's row
/// found (to the transcript's end for the last), blank rows at its end
/// aside.
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
    let joined = join(&off);
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
            // The reply runs to the next turn's row that WAS found: a turn
            // whose row is nowhere (it was queued, or a restart took it) has
            // its reply inside this count, and is named.
            let next = found[i + 1..].iter().position(Option::is_some);
            let end = match next {
                Some(k) => found[i + 1 + k].unwrap_or(rows.len()),
                None => rows.len(),
            };
            // With no later row found at all, every turn after this one is
            // inside its count.
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

/// The first user row at or after `from` whose text begins with `text`'s
/// first 60 characters (whitespace aside), else — for a paste-like text —
/// the first `[Pasted text …]` row there that fits it.
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

// ------------------------------------------------------------------ the summary

/// The numbers on top.
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
    pub complete: usize,
    pub incomplete: usize,
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
    /// The numbers (see [`Summary`]). Latency needs the clock: without it,
    /// none is measured.
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
            match r.complete {
                Some(true) => s.complete += 1,
                Some(false) => s.incomplete += 1,
                None => {}
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

    /// The timeline, in time order (a carried turn, with no time, first).
    pub fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for t in &self.turns {
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
                what: format!("turn {} sent: {}", t.id, cut(&t.text, TURN_CHARS)),
                full: format!(
                    "turn {} sent{}: {}",
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

/// A timeline row's lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    Manager = 0,
    Worker = 1,
    Watcher = 2,
    Fabric = 3,
}

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Lane::Manager => "manager",
            Lane::Worker => "worker",
            Lane::Watcher => "watcher",
            Lane::Fabric => "fabric",
        }
    }
}

/// What a timeline row is, for the HTML's marks.
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
    Approved,
    Dismissed,
    Reconnect,
    End,
    MailIn,
    MailOut,
    Other,
}

impl MarkKind {
    fn of_record(r: &JournalRecord) -> Self {
        match (r.kind.as_str(), r.phase.as_str()) {
            ("event", "idle" | "turn" | "idle-no-report" | "resumed") => MarkKind::Idle,
            ("event", "question") => MarkKind::Question,
            ("event", "prompt") => MarkKind::Prompt,
            ("event", "limited" | "still-limited") => MarkKind::Limited,
            ("event", "survey") => MarkKind::Survey,
            ("event", "context" | "compacted") => MarkKind::Context,
            ("approved", _) => MarkKind::Approved,
            ("dismissed", _) => MarkKind::Dismissed,
            ("reconnect", _) => MarkKind::Reconnect,
            ("timeout" | "exit", _) => MarkKind::End,
            _ => MarkKind::Other,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            MarkKind::Turn => "turn",
            MarkKind::Reply => "reply",
            MarkKind::Idle => "idle",
            MarkKind::Question => "question",
            MarkKind::Prompt => "prompt",
            MarkKind::Limited => "limited",
            MarkKind::Survey => "survey",
            MarkKind::Context => "context",
            MarkKind::Approved => "approved",
            MarkKind::Dismissed => "dismissed",
            MarkKind::Reconnect => "reconnect",
            MarkKind::End => "end",
            MarkKind::MailIn => "mail-in",
            MarkKind::MailOut => "mail-out",
            MarkKind::Other => "other",
        }
    }
}

/// One timeline row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub t: Option<i64>,
    pub lane: Lane,
    pub mark: MarkKind,
    /// The WHAT column (cut for a terminal).
    pub what: String,
    /// The whole of it (the HTML's hover).
    pub full: String,
    /// The DURATION/LATENCY column.
    pub dur: String,
    /// A turn's bar: where the `turn` verb settled.
    pub end: Option<i64>,
    /// A turn's bar: where the worker stopped (the journal's EVENT).
    pub long_end: Option<i64>,
}

// ------------------------------------------------------------------ time words

/// `1.8s`, `950ms`.
fn ms(n: u64) -> String {
    if n < 1000 {
        format!("{n}ms")
    } else {
        format!("{}.{}s", n / 1000, (n % 1000) / 100)
    }
}

/// `42s`, `3m10s`, `2h05m`.
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

/// A Unix ms time in local time: `HH:MM:SS`, or `YYYY-MM-DD HH:MM:SS` with
/// `date`.
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

/// `UTC-07:00` for an offset in seconds.
pub fn zone(offset_s: i64) -> String {
    let sign = if offset_s < 0 { '-' } else { '+' };
    let a = offset_s.abs();
    format!("UTC{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60)
}

/// `--since`: Unix milliseconds, or an ISO date or date-time —
/// `2026-09-14` (local midnight), `2026-09-14T10:30[:05]` or with a space
/// for the `T` (local), with `Z` or `±HH:MM` (that zone).
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
                None if z.len() == 4 => z.split_at(2),
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
    Some((days_from_civil(y, mo, d) * 86_400 + secs - offset) * 1000)
}

// ------------------------------------------------------------------ text and md

/// The time column: `HH:MM:SS`, with the date when the rows span more than
/// one local day; `-` for none; `~` before a time the clock could not place.
fn time_cell(t: Option<i64>, l: &Ledger, dated: bool) -> String {
    match t {
        None => "-".to_string(),
        Some(t) => {
            let s = stamp(t, l.tz_offset_s, dated);
            if l.aligned { s } else { format!("~{s}") }
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

/// The SUMMARY rows, as (label, value) pairs.
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
    } else if !l.aligned {
        "- (the clock could not be placed)".to_string()
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
        (
            "reports",
            j(format!(
                "{} complete · {} incomplete",
                s.complete, s.incomplete
            )),
        ),
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

/// The text form: aligned columns.
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
    let tw = if dated { 20 } else { 9 } + usize::from(!l.aligned);
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

/// The markdown form: a heading, then tables.
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

/// A string for a JSON document embedded in HTML: [`json_str`], and `<`,
/// `>`, `&`, U+2028 and U+2029 as `\u` escapes, so no `</script>` and no
/// line terminator a script would choke on survives.
pub(super) fn html_json_str(s: &str) -> String {
    json_str(s)
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Text for HTML: `&`, `<`, `>`, `"` and `'` escaped.
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

/// Render `l` as `format`.
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
