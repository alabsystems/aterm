// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ASK ARM (B11): what agents and people wait on, on one node. An ask's
//! release (`post kind=ask` waits for its landing by default) against a
//! `kind=note --wait`'s, idle and under load; a receipt (`--wait-ack`); an ask
//! to a person, which aterm's table escalates to a full flush before its ack,
//! without and with `aterm link notify` paging it; and what that flush costs
//! the ordinary messages beside it. A world of its own with the bridge's wait
//! tally on, so each leg's windows say where the bridge's loop spent a post.
//! Measures, never asserts a floor.

use std::path::{Path, PathBuf};
use std::time::Duration;

use astream_broker::{Broker, FlushCounts};
use aterm_json::{Value, json};
use aterm_link::bridge::IDLE_ACK_GRACE;
use aterm_link::ctl::Ctl;
use aterm_link::state::StateDir;
use aterm_link::wait_tally::{self, Row as TallyRow, Tally, Unit};

use super::pingpong::{describe, ns_ms};
use super::{
    DEADLINE, FLEET, Ruler, World, await_durable, await_kinds, boot_world, bystanders, calibrate,
    consume_kinds, flushes_between, flushes_json, fmt_x, list_since, ms, ms_opt, newest_id,
    per_unit, post, request, sampled, served_line, spawn, stop_and_read, tally, wait_live,
    yardstick, yardstick_json, yardstick_rows,
};
use crate::census;
use crate::clock::{Clock, Heads};
use crate::harness;
use crate::spec::Spec;
use crate::stats;

/// The deadline every ask and task of the arm carries: past any run, so none
/// expires inside one.
pub(super) const DL_MS: u64 = 600_000;

/// The person every person-lane post goes to. aterm's table escalates the
/// lane by its subject (`/f/*/in/p/>`), not by the `h-` principal prefix,
/// which names the sender.
pub(super) const PERSON: &str = "h-bench";

/// The sessions that post asks at once in the busy leg.
const BUSY_SENDERS: usize = 4;

/// How often a tally or the pager's journal is read while the bench waits for it.
const POLL: Duration = Duration::from_micros(250);

/// What a waiting post answered: `OK <id> off=<n>[ dup=1][ ack=<verdict> msg=<row>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Answer {
    pub id: u64,
    pub off: u64,
    pub ack: Option<String>,
    pub msg: Option<u64>,
}

/// Read a waiting post's answer; `None` for anything but an `OK` with its offset.
pub(super) fn answer_of(header: &str) -> Option<Answer> {
    let rest = header.strip_prefix("OK ")?;
    let id = rest.split_whitespace().next()?.parse().ok()?;
    let off = harness::kv(header, "off")?.parse().ok()?;
    Some(Answer {
        id,
        off,
        ack: harness::kv(header, "ack").map(str::to_string),
        msg: harness::kv(header, "msg").and_then(|m| m.parse().ok()),
    })
}

/// One post the bench waited on: before the request, after its answer, and the answer.
#[derive(Debug, Clone)]
pub(super) struct Waited {
    pub t_call: u64,
    pub t_ret: u64,
    pub answer: Answer,
}

/// Send `line`, a post that waits, and read its answer.
///
/// # Panics
///
/// An answer that is not `OK <id> off=<n>`.
pub(super) fn post_waiting(ctl: &mut Ctl, clock: Clock, line: &str) -> Waited {
    let t_call = clock.us();
    let reply = request(ctl, line);
    let t_ret = clock.us();
    let answer = answer_of(reply.header()).unwrap_or_else(|| panic!("{line}: {}", reply.header()));
    Waited {
        t_call,
        t_ret,
        answer,
    }
}

/// The order `arms` variants run in, `n` posts each over two blocks: forward,
/// then backward (A B B A; A B C C B A), as `(variant, posts)`. A block of no
/// posts is left out.
pub(super) fn blocks(n: usize, arms: usize) -> Vec<(usize, usize)> {
    let (first, second) = (n.div_ceil(2), n / 2);
    (0..arms)
        .map(|v| (v, first))
        .chain((0..arms).rev().map(|v| (v, second)))
        .filter(|&(_, posts)| posts > 0)
        .collect()
}

/// The bridge `pid`'s tally at `path`, read now.
fn read_tally(path: &Path, pid: i32) -> Result<Tally, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("no tally at {}: {e}", path.display()))?;
    let tally = Tally::parse(&text)?;
    if i64::from(tally.pid) != i64::from(pid) {
        return Err(format!(
            "the tally is pid {}'s, not the bridge's {pid}",
            tally.pid
        ));
    }
    Ok(tally)
}

/// Wait for the bridge `pid`'s tally at `path` to pass `ready`: a write at an
/// idle round after what the bench waits for.
///
/// # Panics
///
/// After [`DEADLINE`] — the hang detector — with why the last read was not it.
pub(super) fn await_tally_where(
    path: &Path,
    pid: i32,
    what: &str,
    ready: impl Fn(&Tally) -> Result<(), String>,
) -> Tally {
    let mut last = String::new();
    aterm_spec::wait::within(DEADLINE, POLL, || {
        read_tally(path, pid)
            .and_then(|t| ready(&t).map(|()| t))
            .map_err(|why| last = why)
            .ok()
    })
    .unwrap_or_else(|| {
        panic!(
            "{what}: no wait tally at {} was ready within {DEADLINE:?}; the last read: {last}",
            path.display()
        )
    })
}

/// A tally past the inbox commit that covers `off`, and past `posts` posts
/// retired (`outbox sent`, every post's, counted from the bridge's start).
pub(super) fn settled(off: u64, posts: u64) -> impl Fn(&Tally) -> Result<(), String> {
    move |t| {
        let sent = t.row(TallyRow::LaneSent).count;
        if t.committed < off {
            Err(format!(
                "its cursor is at {}, not yet through {off}",
                t.committed
            ))
        } else if sent < posts {
            Err(format!("it has retired {sent} posts, not yet {posts}"))
        } else {
            Ok(())
        }
    }
}

/// A tally and the broker's flushes, read together between two posts.
#[derive(Debug, Clone)]
pub(super) struct Mark {
    pub tally: Tally,
    pub flushes: FlushCounts,
}

/// What one or more windows cost, as the bridge's tally and the broker counted it.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Spent {
    /// Each row's `(count, total)`, by [`TallyRow::index`].
    pub rows: [(u64, u64); wait_tally::ROWS],
    /// The bridge's own clock over the windows, in nanoseconds.
    pub at_ns: u64,
    /// Whether every window counted the state dir's rows.
    pub dir: bool,
    pub flushes: FlushCounts,
    pub windows: usize,
}

impl Spent {
    /// Add the window from `from` to `to`.
    ///
    /// # Panics
    ///
    /// Two tallies that do not pair: another bridge's, or one earlier than the first.
    pub fn add(&mut self, from: &Mark, to: &Mark, what: &str) {
        let d = to
            .tally
            .since(&from.tally)
            .unwrap_or_else(|e| panic!("{what}: the bridge's wait tallies do not pair: {e}"));
        self.add_delta(&d, flushes_between(from.flushes, to.flushes));
    }

    fn add_delta(&mut self, d: &Tally, f: FlushCounts) {
        for row in TallyRow::ALL {
            let h = d.row(row);
            let slot = &mut self.rows[row.index()];
            slot.0 += h.count;
            slot.1 += h.total;
        }
        self.at_ns += d.at_ns;
        self.dir = if self.windows == 0 {
            d.dir
        } else {
            self.dir && d.dir
        };
        self.flushes = FlushCounts {
            barrier: self.flushes.barrier + f.barrier,
            full: self.flushes.full + f.full,
            skipped: self.flushes.skipped + f.skipped,
            failed: self.flushes.failed + f.failed,
        };
        self.windows += 1;
    }

    pub fn count(&self, row: TallyRow) -> u64 {
        self.rows[row.index()].0
    }

    pub fn ns(&self, row: TallyRow) -> u64 {
        self.rows[row.index()].1
    }
}

/// `v / n`, or `None` for no posts.
fn per(v: f64, n: usize) -> Option<f64> {
    (n > 0).then(|| v / n as f64)
}

fn fmt3(v: Option<f64>) -> String {
    v.map_or("–".to_string(), |v| format!("{v:.3}"))
}

/// The tally's time rows, per post, for several columns (`(name, spent,
/// posts)`), with the second column less the first when there are two: the
/// rows either counted, and the loop's own clock.
pub(super) fn steps_rows(columns: &[(&str, &Spent, usize)]) -> Vec<String> {
    let diff = columns.len() == 2;
    let mut head = "| the loop's step (wait tally v=2) | partitions |".to_string();
    let mut rule = "|---|---|".to_string();
    for (name, _, _) in columns {
        head.push_str(&format!(" {name}: count / post | {name}: ms / post |"));
        rule.push_str("---:|---:|");
    }
    if diff {
        head.push_str(&format!(
            " {} − {}: ms / post |",
            columns[1].0, columns[0].0
        ));
        rule.push_str("---:|");
    }
    let mut rows = vec![head, rule];
    for row in TallyRow::ALL {
        if row.unit() != Unit::Ns || columns.iter().all(|(_, s, _)| s.count(row) == 0) {
            continue;
        }
        let mut line = format!(
            "| {} | {} |",
            describe(row),
            if row.partitions() { "yes" } else { "" }
        );
        let mut ms_each = Vec::new();
        for (_, s, n) in columns {
            let ms_per = per(ns_ms(s.ns(row)), *n);
            ms_each.push(ms_per);
            line.push_str(&format!(
                " {} | {} |",
                fmt3(per(s.count(row) as f64, *n)),
                fmt3(ms_per)
            ));
        }
        if diff {
            let d = ms_each[1].zip(ms_each[0]).map(|(b, a)| b - a);
            line.push_str(&format!(
                " {} |",
                d.map_or("–".to_string(), |d| format!("{d:+.3}"))
            ));
        }
        rows.push(line);
    }
    let mut clock = "| **the bridge's own clock** | |".to_string();
    for (_, s, n) in columns {
        clock.push_str(&format!(" | {} |", fmt3(per(ns_ms(s.at_ns), *n))));
    }
    if diff {
        clock.push_str(" |");
    }
    rows.push(clock);
    if columns.iter().any(|(_, s, _)| !s.dir) {
        rows.push(String::new());
        rows.push(
            "The state dir's rows were not counted in every window: a build without debug \
             assertions."
                .to_string(),
        );
    }
    rows
}

/// [`steps_rows`]'s numbers for the report's JSON.
pub(super) fn steps_json(s: &Spent, n: usize) -> Value {
    let mut rows = aterm_json::Map::new();
    for row in TallyRow::ALL {
        let (count, total) = s.rows[row.index()];
        if count == 0 {
            continue;
        }
        rows.insert(
            row.name().to_string(),
            json!({
                "count": count,
                "count_per_post": per(count as f64, n).map(stats::round3),
                "ms_per_post": (row.unit() == Unit::Ns)
                    .then(|| per(ns_ms(total), n).map(stats::round3))
                    .flatten(),
            }),
        );
    }
    json!({
        "posts": n,
        "windows": s.windows,
        "dir_counted": s.dir,
        "bridge_ms_per_post": per(ns_ms(s.at_ns), n).map(stats::round3),
        "rows": Value::Object(rows),
        "flushes": flushes_json(s.flushes, n),
    })
}

/// A broker flush line per post: barrier and full.
pub(super) fn flushes_per_post(what: &str, f: FlushCounts, n: usize) -> String {
    format!(
        "{what}: {} barrier ({} per post) and {} full ({} per post), {} full skipped as covered.",
        f.barrier,
        fmt3(per(f.barrier as f64, n)),
        f.full,
        fmt3(per(f.full as f64, n)),
        f.skipped
    )
}

/// The table head [`row_u`] fills.
pub(super) const HEAD_U: [&str; 2] = [
    "| measure (ms) | n | mean | p50 | p95 | p99 | p50 in publishes |",
    "|---|---:|---:|---:|---:|---:|---:|",
];

/// [`stats::row`] with the p50 in strict publishes beside it.
pub(super) fn row_u(what: &str, values: &[f64], unit: Option<f64>) -> String {
    let x = per_unit(stats::summarize(values).map(|s| s.p50), unit);
    format!("{} {} |", stats::row(what, values), fmt_x(x))
}

/// [`stats::to_json`] with the p50 in strict publishes.
pub(super) fn json_u(values: &[f64], unit: Option<f64>) -> Value {
    let mut v = stats::to_json(values);
    if let Some(fields) = v.as_object_mut() {
        let x = per_unit(stats::summarize(values).map(|s| s.p50), unit);
        fields.insert("p50_in_publishes".to_string(), json!(x));
    }
    v
}

/// The releases later than `threshold` ms after their ack: `(late, of)`.
pub(super) fn late(acked_to_released: &[f64], threshold: f64) -> (usize, usize) {
    (
        acked_to_released.iter().filter(|&&v| v > threshold).count(),
        acked_to_released.len(),
    )
}

fn late_json((late, of): (usize, usize), threshold: f64) -> Value {
    json!({
        "threshold_ms": stats::round3(threshold),
        "late": late,
        "of": of,
        "share": (of > 0).then(|| stats::round3(late as f64 / of as f64)),
    })
}

fn share_text((late, of): (usize, usize)) -> String {
    if of == 0 {
        "–".to_string()
    } else {
        format!("{late} of {of} ({:.1}%)", 100.0 * late as f64 / of as f64)
    }
}

/// The kinds the idle leg compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Note,
    Ask,
}

impl Kind {
    const BOTH: [Kind; 2] = [Kind::Note, Kind::Ask];

    const fn word(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Ask => "ask",
        }
    }

    /// `from` posts `text` to `to`, waiting for its landing: an ask by
    /// default, a note by `--wait`.
    fn line(self, from: &str, to: &str, text: &str) -> String {
        match self {
            Kind::Note => format!("@{from} post to=@{to} kind=note --wait -- {text}"),
            Kind::Ask => format!("@{from} post to=@{to} kind=ask dl={DL_MS} -- {text}"),
        }
    }
}

/// What every leg runs against.
struct Stage<'a> {
    w: &'a World,
    broker: &'a Broker,
    clock: Clock,
    a: String,
    b: String,
    tally: PathBuf,
    pid: i32,
}

impl Stage<'_> {
    /// The tally once `ready`, and the broker's flushes beside it.
    fn mark(&self, what: &str, ready: impl Fn(&Tally) -> Result<(), String>) -> Mark {
        let tally = await_tally_where(&self.tally, self.pid, what, ready);
        Mark {
            tally,
            flushes: self.broker.flush_counts(),
        }
    }

    /// Every post the bridge has retired so far, by the tally `m`.
    fn retired(m: &Mark) -> u64 {
        m.tally.row(TallyRow::LaneSent).count
    }

    /// List `sid`'s rows past `since`, which must be exactly one, `text`,
    /// row `woke`: its offset.
    fn one_row(&self, ctl: &mut Ctl, sid: &str, since: &mut u64, woke: u64, text: &str) -> u64 {
        let rows = list_since(ctl, sid, *since);
        assert!(
            rows.len() == 1 && rows[0].text == text && rows[0].id == woke,
            "`{text}`: @{sid} woke at row {woke} and listed {rows:?}"
        );
        *since = woke;
        rows[0].off
    }
}

/// One release the idle leg timed.
#[derive(Debug, Clone)]
struct Release {
    waited: Waited,
    /// B's `await inbox` answering with it.
    t_seen: u64,
}

/// The idle leg, by [`Kind::BOTH`]'s index.
struct Idle {
    releases: [Vec<Release>; 2],
    spent: [Spent; 2],
    /// Each block's first and last offset.
    windows: [Vec<(u64, u64)>; 2],
}

fn idle(s: &Stage, spec: &Spec, ctl: &mut Ctl, b_since: &mut u64) -> (Idle, Mark) {
    let mut one = |kind: Kind, text: String| -> (Release, Mark) {
        let waited = post_waiting(ctl, s.clock, &kind.line(&s.a, &s.b, &text));
        let (woke, t_seen) = await_kinds(ctl, s.clock, &s.b, *b_since, kind.word())
            .unwrap_or_else(|e| panic!("`{text}`: {e}"));
        let off = s.one_row(ctl, &s.b, b_since, woke, &text);
        assert_eq!(
            off, waited.answer.off,
            "`{text}`: the row is the record the post answered"
        );
        let mark = s.mark(&format!("`{text}`'s commit"), settled(off, 0));
        (Release { waited, t_seen }, mark)
    };
    let mut prev = None;
    for i in 0..spec.warmup {
        for kind in Kind::BOTH {
            prev = Some(one(kind, format!("rel-warm-{}-{i}", kind.word())).1);
        }
    }
    let mut prev = prev.unwrap_or_else(|| s.mark("the idle leg's start", |_| Ok(())));
    let mut out = Idle {
        releases: [Vec::new(), Vec::new()],
        spent: [Spent::default(); 2],
        windows: [Vec::new(), Vec::new()],
    };
    let mut i = 0;
    for (v, posts) in blocks(spec.asks, 2) {
        let kind = Kind::BOTH[v];
        let mut first = None;
        let mut last = None;
        for _ in 0..posts {
            let (r, m) = one(kind, format!("rel-{}-{i}", kind.word()));
            i += 1;
            first.get_or_insert(r.waited.answer.off);
            last = Some((r.waited.answer.off, m));
            out.releases[v].push(r);
        }
        let (last_off, end) = last.expect("a block posts at least once");
        out.spent[v].add(&prev, &end, "the idle leg");
        out.windows[v].push((first.unwrap_or(last_off), last_off));
        prev = end;
    }
    (out, prev)
}

/// The busy leg's senders: `(message index, its post)`, and what they cost.
struct Busy {
    posted: Vec<(usize, Waited)>,
    /// Each message's arrival in B's inbox, by index.
    seen: Vec<u64>,
    spent: Spent,
}

fn busy_senders(
    s: &Stage,
    n: usize,
    senders: &[String],
    b_since: &mut u64,
    start: Mark,
) -> (Busy, Mark) {
    let since = *b_since;
    let base = Stage::retired(&start);
    let (posted, consumed) = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut ctl = s.w.ctl();
            consume_kinds(&mut ctl, s.clock, &s.b, since, ("busy-", "ask"), n, None)
        });
        let handles: Vec<_> = senders
            .iter()
            .enumerate()
            .map(|(k, sid)| {
                scope.spawn(move || {
                    let mut ctl = s.w.ctl();
                    (k..n)
                        .step_by(senders.len())
                        .map(|j| {
                            let line = Kind::Ask.line(sid, &s.b, &format!("busy-{j}"));
                            (j, post_waiting(&mut ctl, s.clock, &line))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut posted = Vec::with_capacity(n);
        for h in handles {
            posted.extend(h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)));
        }
        let consumed = reader
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        (posted, consumed)
    });
    let consumed = consumed.unwrap_or_else(|e| panic!("the busy senders: {e}"));
    let seen = tally(&consumed.seen, n)
        .unwrap_or_else(|e| panic!("the busy senders: every ask is seen exactly once, but {e}"));
    *b_since = consumed.since;
    let last_off = posted.iter().map(|(_, p)| p.answer.off).max().unwrap_or(0);
    let end = s.mark(
        "the busy senders' last commit",
        settled(last_off, base + n as u64),
    );
    let mut spent = Spent::default();
    spent.add(&start, &end, "the busy senders");
    (
        Busy {
            posted,
            seen: seen.iter().map(|x| x.t).collect(),
            spent,
        },
        end,
    )
}

/// One busy ping-pong round: A asks, B answers with a task.
#[derive(Debug, Clone)]
struct Round {
    ping: Waited,
    t_b_seen: u64,
    pong: Waited,
    t_a_seen: u64,
}

fn busy_pingpong(
    s: &Stage,
    spec: &Spec,
    ctl: &mut Ctl,
    (a_since, b_since): (&mut u64, &mut u64),
    start: Mark,
) -> (Vec<Round>, Spent, Mark) {
    let base = Stage::retired(&start);
    let mut one = |i: usize| -> Round {
        let (ping_text, pong_text) = (format!("pa-{i}"), format!("pt-{i}"));
        let ping = post_waiting(ctl, s.clock, &Kind::Ask.line(&s.a, &s.b, &ping_text));
        let (woke, t_b_seen) = await_kinds(ctl, s.clock, &s.b, *b_since, "ask")
            .unwrap_or_else(|e| panic!("`{ping_text}`: {e}"));
        s.one_row(ctl, &s.b, b_since, woke, &ping_text);
        let pong = post_waiting(
            ctl,
            s.clock,
            &format!(
                "@{} post to=@{} kind=task dl={DL_MS} -- {pong_text}",
                s.b, s.a
            ),
        );
        // A task from a node outside `--accept-from` is delivered as a note
        // (`bridge.rs` `DEMOTE_UNLESS_ACCEPTED`); its sender waited as for a task.
        let (woke, t_a_seen) = await_kinds(ctl, s.clock, &s.a, *a_since, "task,note")
            .unwrap_or_else(|e| panic!("`{pong_text}`: {e}"));
        s.one_row(ctl, &s.a, a_since, woke, &pong_text);
        Round {
            ping,
            t_b_seen,
            pong,
            t_a_seen,
        }
    };
    let mut posts = 0;
    for i in 0..spec.warmup {
        let _ = one(i);
        posts += 2;
    }
    let start = if spec.warmup == 0 {
        start
    } else {
        s.mark("the busy ping-pong's warm-up", settled(0, base + posts))
    };
    let rounds: Vec<Round> = (spec.warmup..spec.warmup + spec.asks)
        .map(&mut one)
        .collect();
    posts += 2 * rounds.len() as u64;
    let last = rounds.last().expect("at least one round");
    let end = s.mark(
        "the busy ping-pong's last commit",
        settled(last.pong.answer.off, base + posts),
    );
    let mut spent = Spent::default();
    spent.add(&start, &end, "the busy ping-pong");
    (rounds, spent, end)
}

/// One receipt: A's ask parked for B's verdict, and B's side of it.
#[derive(Debug, Clone)]
struct Receipt {
    waited: Waited,
    /// B's `await inbox` answering with the ask.
    t_b_seen: u64,
    /// B's `inbox seen <id> handled`: before the request, after its `OK`.
    t_verdict: u64,
    t_verdict_ok: u64,
    /// The ack's record, from A's listing of it.
    ack_off: u64,
}

fn receipts(
    s: &Stage,
    spec: &Spec,
    ctl: &mut Ctl,
    (a_since, b_since): (&mut u64, &mut u64),
    start: Mark,
) -> (Vec<Receipt>, Spent, Mark) {
    let n = spec.asks;
    let base = Stage::retired(&start);
    let since = *b_since;
    let (mine, theirs, end) = std::thread::scope(|scope| {
        let acker = scope.spawn(move || {
            let mut ctl = s.w.ctl();
            let mut since = since;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let text = format!("rcpt-{i}");
                let (woke, t_b_seen) = await_kinds(&mut ctl, s.clock, &s.b, since, "ask")
                    .unwrap_or_else(|e| panic!("`{text}` at B: {e}"));
                s.one_row(&mut ctl, &s.b, &mut since, woke, &text);
                let t_verdict = s.clock.us();
                let r = request(&mut ctl, &format!("@{} inbox seen {woke} handled", s.b));
                let t_verdict_ok = s.clock.us();
                assert!(
                    r.ok(),
                    "`{text}`: inbox seen {woke} handled: {}",
                    r.header()
                );
                out.push((t_b_seen, t_verdict, t_verdict_ok));
            }
            (out, since)
        });
        let mut mine = Vec::with_capacity(n);
        let mut end = None;
        for i in 0..n {
            let text = format!("rcpt-{i}");
            let waited = post_waiting(
                ctl,
                s.clock,
                &format!(
                    "@{} post to=@{} kind=ask dl={DL_MS} --wait-ack={} -- {text}",
                    s.a,
                    s.b,
                    DEADLINE.as_millis()
                ),
            );
            assert_eq!(
                waited.answer.ack.as_deref(),
                Some("handled"),
                "`{text}`: the post answers B's verdict"
            );
            let msg = waited.answer.msg.expect("a receipt names its row");
            let rows = list_since(ctl, &s.a, *a_since);
            let ack = rows.iter().find(|r| r.id == msg).unwrap_or_else(|| {
                panic!("`{text}`: A's listing lacks the ack row {msg}: {rows:?}")
            });
            let ack_off = ack.off;
            *a_since = rows.iter().map(|r| r.id).max().unwrap_or(*a_since);
            end = Some(s.mark(
                &format!("`{text}`'s receipt committed"),
                settled(ack_off, base + i as u64 + 1),
            ));
            mine.push((waited, ack_off));
        }
        let (theirs, since) = acker
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        *b_since = since;
        (mine, theirs, end)
    });
    let end = end.expect("at least one receipt");
    let mut spent = Spent::default();
    spent.add(&start, &end, "the receipts");
    let out = mine
        .into_iter()
        .zip(theirs)
        .map(
            |((waited, ack_off), (t_b_seen, t_verdict, t_verdict_ok))| Receipt {
                waited,
                t_b_seen,
                t_verdict,
                t_verdict_ok,
                ack_off,
            },
        )
        .collect();
    (out, spent, end)
}

/// The person leg's two variants, run in blocks A B B A: the escalation
/// alone, and the same asks with `aterm link notify` paging each on this
/// machine.
const PERSON_ARMS: [&str; 2] = ["unpaged", "paged"];

/// The least time from one person-lane ask's call to the next, in both of
/// the leg's variants, so the two post at one cadence. It is longer than a
/// page's whole path takes here (the cursor written 32-35 ms after the call
/// at p50, `2026-10-26-fabric-185` to `-192`), so the paged variant's wait
/// for its pager seldom stretches it; the report gives both variants' gaps.
const PERSON_PACE: Duration = Duration::from_millis(60);

/// `aterm link notify` following the person's asks and paging each with
/// `true`: its journal's `<off> fired` line says when, and its cursor moving
/// past the record says it has written the last of that page's files.
struct Pager {
    child: Option<aterm_uds::spawnfd::Child>,
    journal: PathBuf,
    cursor: PathBuf,
    out: PathBuf,
}

impl Pager {
    /// Start one that pages the person's asks from offset `since` on.
    fn start(w: &World, since: u64) -> Pager {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = w.tmp.join("pager");
        std::fs::create_dir_all(&dir).expect("the pager's state dir");
        let cap = dir.join("person.cap");
        let caps: String = aterm_link::enable::person_grants(FLEET, PERSON)
            .iter()
            .map(|g| harness::cap_line(g))
            .collect();
        std::fs::write(&cap, caps).expect("write the person's cap");
        std::fs::set_permissions(&cap, std::fs::Permissions::from_mode(0o600))
            .expect("the person's cap is 0600");
        let out = dir.join("follow.out");
        let log = std::fs::File::create(&out).expect("the pager's output");
        let err = log.try_clone().expect("the pager's output, twice");
        let sel = aterm_link::notify::Sel::Ask(PERSON.to_string());
        let mut cmd = aterm_uds::spawnfd::command(harness::aterm_to_launch());
        cmd.arg("link")
            .args(["notify", "--fleet", FLEET, "--broker", &w.broker_sock])
            .arg("--cap-file")
            .arg(&cap)
            .arg("--state")
            .arg(&dir)
            .args(["--on", &sel.label(), "--exec", "true"])
            .args(["--since", &since.to_string()])
            .args(["--rate", "1000000/1s"])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(log)
            .stderr(err);
        Pager {
            child: Some(cmd.spawn().expect("spawn aterm link notify")),
            journal: dir.join("notify/journal"),
            cursor: dir.join("notify/cur").join(sel.key()),
            out,
        }
    }

    /// When `ready` first holds of what the pager wrote, read every [`POLL`].
    ///
    /// # Panics
    ///
    /// After [`DEADLINE`], naming `what`, with the pager's output.
    fn until(&self, clock: Clock, what: &str, ready: impl Fn() -> bool) -> u64 {
        aterm_spec::wait::within(DEADLINE, POLL, || ready().then(|| clock.us())).unwrap_or_else(
            || {
                panic!(
                    "the pager did not {what} within {DEADLINE:?}; its output:\n{}",
                    std::fs::read_to_string(&self.out).unwrap_or_default()
                )
            },
        )
    }

    /// When the journal first says `off` was paged (`<off> fired …`).
    fn paged(&self, clock: Clock, off: u64) -> u64 {
        let want = format!("{off} fired ");
        self.until(clock, &format!("page @{off}"), || {
            std::fs::read_to_string(&self.journal)
                .is_ok_and(|j| j.lines().any(|l| l.starts_with(&want)))
        })
    }

    /// When the cursor first stands past `off`: the page's last write.
    fn quiet(&self, clock: Clock, off: u64) -> u64 {
        self.until(clock, &format!("move its cursor past @{off}"), || {
            std::fs::read_to_string(&self.cursor)
                .ok()
                .and_then(|c| c.trim().parse::<u64>().ok())
                .is_some_and(|at| at > off)
        })
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One person-lane ask: its release, the time since the ask before it, and,
/// in the paged variant, when the pager paged it and went quiet after.
#[derive(Debug, Clone)]
struct PersonAsk {
    waited: Waited,
    /// Call to call from the ask before, in ms.
    gap: Option<f64>,
    /// `(paged, quiet)` on the bench clock.
    pager: Option<(u64, u64)>,
}

/// The person leg's two variants ([`PERSON_ARMS`]).
struct Person {
    asks: [Vec<PersonAsk>; 2],
    spent: [Spent; 2],
}

/// Sleep until `pace` after `from` (bench-clock µs), if that is still ahead.
fn pace_from(clock: Clock, from: u64, pace: Duration) {
    let due = from.saturating_add(u64::try_from(pace.as_micros()).unwrap_or(u64::MAX));
    let now = clock.us();
    if due > now {
        std::thread::sleep(Duration::from_micros(due - now));
    }
}

fn person(s: &Stage, spec: &Spec, ctl: &mut Ctl, start: Mark) -> (Person, Mark) {
    let base = Stage::retired(&start);
    let mut posts = 0;
    let mut out = Person {
        asks: [Vec::new(), Vec::new()],
        spent: [Spent::default(); 2],
    };
    let mut pager: Option<Pager> = None;
    let mut since = 0;
    let mut last_call = None;
    let mut prev = start;
    let mut k = 0;
    let warm = spec.warmup.max(1);
    for (v, n) in blocks(spec.asks, 2) {
        if v == 0 {
            pager = None;
        } else if pager.is_none() {
            pager = Some(Pager::start(s.w, since));
        }
        let mut from = None;
        for i in 0..warm + n {
            if let Some(t) = last_call {
                pace_from(s.clock, t, PERSON_PACE);
            }
            let waited = post_waiting(
                ctl,
                s.clock,
                &format!(
                    "@{} post to={PERSON} kind=ask dl={DL_MS} -- person-{}-{k}",
                    s.a, PERSON_ARMS[v]
                ),
            );
            k += 1;
            let gap = last_call.map(|t| ms(t, waited.t_call));
            last_call = Some(waited.t_call);
            let off = waited.answer.off;
            since = off + 1;
            let paged = pager
                .as_ref()
                .map(|p| (p.paged(s.clock, off), p.quiet(s.clock, off)));
            posts += 1;
            let m = s.mark("a person-lane ask retired", settled(0, base + posts));
            if i + 1 < warm {
                continue;
            }
            if i + 1 == warm {
                from = Some(m);
                continue;
            }
            out.asks[v].push(PersonAsk {
                waited,
                gap,
                pager: paged,
            });
            if i + 1 == warm + n {
                let from = from.as_ref().expect("a block warms up first");
                out.spent[v].add(from, &m, "the person leg");
                prev = m;
            }
        }
    }
    drop(pager);
    (out, prev)
}

/// One ordinary message of the mix leg.
#[derive(Debug, Clone)]
struct Plain {
    t_call: u64,
    t_ret: u64,
    t_seen: u64,
    off: u64,
    /// A person-lane post went out just before it.
    after_person: bool,
}

/// The mix leg's two variants, without and with the person-lane posts.
struct Mix {
    messages: [Vec<Plain>; 2],
    spent: [Spent; 2],
    person_posts: usize,
}

fn mix(
    s: &Stage,
    spec: &Spec,
    ctl: &mut Ctl,
    c: &str,
    b_since: &mut u64,
    start: Mark,
) -> (Mix, Mark) {
    let mut c_ctl = s.w.ctl();
    let base = Stage::retired(&start);
    let mut posts = 0u64;
    let mut out = Mix {
        messages: [Vec::new(), Vec::new()],
        spent: [Spent::default(); 2],
        person_posts: 0,
    };
    let mut prev = start;
    let mut i = 0;
    for (v, n) in blocks(spec.samples, 2) {
        let mut last = None;
        for k in 0..n {
            let after_person = v == 1 && k % spec.mix_every == 0;
            if after_person {
                let line = format!("@{c} post to={PERSON} kind=note -- mix-person-{i}");
                let r = request(&mut c_ctl, &line);
                assert!(r.ok(), "{line}: {}", r.header());
                out.person_posts += 1;
                posts += 1;
            }
            let text = format!("mix-{v}-{i}");
            i += 1;
            let (t_call, t_ret) = post(ctl, s.clock, &s.a, &s.b, &text);
            posts += 1;
            let (woke, t_seen) = super::await_note(ctl, s.clock, &s.b, *b_since)
                .unwrap_or_else(|e| panic!("`{text}`: {e}"));
            let off = s.one_row(ctl, &s.b, b_since, woke, &text);
            let mark = s.mark(&format!("`{text}`'s commit"), settled(off, base + posts));
            out.messages[v].push(Plain {
                t_call,
                t_ret,
                t_seen,
                off,
                after_person,
            });
            last = Some(mark);
        }
        let end = last.expect("a block posts at least once");
        out.spent[v].add(&prev, &end, "the mix leg");
        prev = end;
    }
    (out, prev)
}

/// The rows of the bridge's asked and deadline tables, read through the
/// bridge's own state API (`StateDir::asked`, `deadlines`): every ask
/// rewrites both whole (`set_asked`, `set_deadlines`).
fn table_rows(state: &StateDir) -> (usize, usize) {
    (state.asked().len(), state.deadlines().len())
}

/// Whether both tables grew by a leg's `asks` unanswered asks (each to its
/// cap, `ASKED_KEEP` and `DEADLINES_KEEP`). A reading that did not means the
/// bench no longer reads the tables where the bridge keeps them.
fn tables_grew(before: (usize, usize), after: (usize, usize), asks: usize) -> Result<(), String> {
    use aterm_link::state::{ASKED_KEEP, DEADLINES_KEEP};
    let want = (
        (before.0 + asks).min(ASKED_KEEP),
        (before.1 + asks).min(DEADLINES_KEEP),
    );
    if after.0 >= want.0 && after.1 >= want.1 {
        Ok(())
    } else {
        Err(format!(
            "the bridge's asked/deadline tables read {}/{} after {asks} unanswered asks, from \
             {}/{}: at least {}/{} were due",
            after.0, after.1, before.0, before.1, want.0, want.1
        ))
    }
}

/// The table readings in one clause: `<asked>/<deadlines> before <leg>, …`.
fn tables_text(tables: &[(&str, (usize, usize))]) -> String {
    tables
        .iter()
        .map(|(at, (asked, deadlines))| {
            if *at == "the end" {
                format!("{asked}/{deadlines} at the end")
            } else {
                format!("{asked}/{deadlines} before {at}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The ask world: boot with the tally on, run legs (a)-(d), read the log.
pub(super) fn measure(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the ask world");
    let mut w = boot_world("fbench-ask", spec, &[(wait_tally::SEAM, "1")]);
    w.wait_ready();
    let served = served_line(&w, spec);
    let (a, b) = w.two_sessions();
    let senders: Vec<String> = (0..BUSY_SENDERS).map(|_| spawn(&w)).collect();
    let c = spawn(&w);
    let mut live = vec![a.clone(), b.clone(), c.clone()];
    live.extend(senders.iter().cloned());
    live.extend(bystanders(&w, spec.bystanders));
    wait_live(
        &w,
        &live,
        0,
        "the ask world's sessions' presence rows, and every bystander's, to say live",
    );
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    let pid = w.bridge_pid();
    let tally_path = w.state.join(wait_tally::FILE);
    let (legs, heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let s = Stage {
            w: &w,
            broker,
            clock,
            a: a.clone(),
            b: b.clone(),
            tally: tally_path.clone(),
            pid,
        };
        let mut ctl = w.ctl();
        let mut a_since = newest_id(&mut ctl, &a);
        let mut b_since = newest_id(&mut ctl, &b);
        eprintln!(
            "fabric bench: asks, idle: {} of each kind ({} warm-up)",
            spec.asks, spec.warmup
        );
        let state = StateDir::open(&w.state).expect("the bridge's state dir");
        let before = table_rows(&state);
        let mut tables = vec![("the idle leg", before)];
        let (idle, m) = idle(&s, spec, &mut ctl, &mut b_since);
        let after = table_rows(&state);
        tables_grew(before, after, spec.asks).unwrap_or_else(|e| panic!("the idle leg: {e}"));
        tables.push(("the busy senders", after));
        eprintln!(
            "fabric bench: asks, busy: {} from {BUSY_SENDERS} senders",
            spec.asks
        );
        let (busy, m) = busy_senders(&s, spec.asks, &senders, &mut b_since, m);
        tables.push(("the busy ping-pong", table_rows(&state)));
        eprintln!("fabric bench: asks, busy: {} ask/task rounds", spec.asks);
        let (rounds, rounds_spent, m) =
            busy_pingpong(&s, spec, &mut ctl, (&mut a_since, &mut b_since), m);
        tables.push(("the receipts", table_rows(&state)));
        eprintln!("fabric bench: asks, receipts: {}", spec.asks);
        let (receipts, receipts_spent, m) =
            receipts(&s, spec, &mut ctl, (&mut a_since, &mut b_since), m);
        tables.push(("the person lane", table_rows(&state)));
        eprintln!(
            "fabric bench: asks, person lane: {} without a pager and {} with one",
            spec.asks, spec.asks
        );
        let (persons, m) = person(&s, spec, &mut ctl, m);
        tables.push(("the mix", table_rows(&state)));
        eprintln!(
            "fabric bench: asks, the escalated mix: {} plain and {} mixed, one person-lane post \
             every {}",
            spec.samples, spec.samples, spec.mix_every
        );
        let (mix, _) = mix(&s, spec, &mut ctl, &c, &mut b_since, m);
        tables.push(("the end", table_rows(&state)));
        await_durable(broker);
        Legs {
            idle,
            busy,
            rounds,
            rounds_spent,
            receipts,
            receipts_spent,
            persons,
            mix,
            tables,
        }
    });
    let cal_after = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let barrier = pooled_p50(&cal_before.ordered, &cal_after.ordered);
    let node = w.node.clone();
    let recs = stop_and_read(&mut w);
    let group = aterm_link::subject::inbox_group(FLEET, &node);
    let ctx = Reading {
        heads: &heads,
        unit,
        threshold: ms_of(IDLE_ACK_GRACE) + barrier.unwrap_or(0.0),
    };
    let mut json = json!({
        "node": node.clone(),
        "served": served.trim_end(),
        "head_changes_sampled": heads.acked.len(),
        "durable_head_changes_sampled": heads.durable.len(),
        "yardstick": yardstick_json(&ruler, &cal_before, &cal_after, unit),
        "late_threshold": {
            "what": "a release later than the bridge's idle ack grace (aterm_link::bridge::IDLE_ACK_GRACE) plus one barrier (the ordered yardstick's p50) after its ack",
            "idle_ack_grace_ms": ms_of(IDLE_ACK_GRACE),
            "barrier_ms": barrier.map(stats::round3),
            "threshold_ms": stats::round3(ctx.threshold),
        },
        "tables": legs
            .tables
            .iter()
            .map(|(at, (asked, deadlines))| json!({ "at": at, "asked": asked, "deadlines": deadlines }))
            .collect::<Vec<_>>(),
        "asks": spec.asks,
        "warmup": spec.warmup,
        "bystanders": spec.bystanders,
    });
    let mut md = vec![
        format!("## Asks: what agents and people wait on (node `{node}`)\n"),
        served,
        format!(
            "A world of its own, the bridge's wait tally on, {} idle bystander session(s). An ask \
             (`post kind=ask dl={DL_MS}`) waits for its landing by default: aterm answers `OK \
             <id> off=<n>` — RELEASES it — once the bridge retires the post (`outbox sent`), \
             after its ack and `post_landed`'s writes (the self-ack append, and for an ask the \
             asked and deadline tables, each rewritten whole). Acked and durable are the broker's \
             heads, as in the messaging section. A release later than {:.3} ms after its ack — \
             the bridge's idle ack grace ({} ms, `IDLE_ACK_GRACE`) plus one barrier ({} ms, the \
             ordered yardstick's p50) — is counted late. Every block waits for the bridge's \
             tally past the last post's inbox commit before the next, so the tally's rows below \
             are each block's own. Every ask rewrites the bridge's asked and deadline tables \
             whole, and they grow over the legs (none of the arm's asks is answered but the \
             receipts', and none expires inside the run): their rows were {}.\n",
            spec.bystanders,
            ctx.threshold,
            ms_of(IDLE_ACK_GRACE),
            fmt_x(barrier),
            tables_text(&legs.tables),
        ),
    ];
    let (idle_json, idle_md) = idle_report(&ctx, &legs.idle, &recs, &group);
    md.extend(idle_md);
    let (busy_json, busy_md) = busy_report(&ctx, &legs);
    md.extend(busy_md);
    let (receipt_json, receipt_md) = receipt_report(&ctx, &legs.receipts, &legs.receipts_spent);
    md.extend(receipt_md);
    let (person_json, person_md) = person_report(&ctx, &legs.persons);
    md.extend(person_md);
    let (mix_json, mix_md) = mix_report(&ctx, spec, &legs.mix);
    md.extend(mix_md);
    md.push(String::new());
    md.extend(yardstick_rows(&ruler, &cal_before, &cal_after));
    if let Some(fields) = json.as_object_mut() {
        fields.insert("release_idle".to_string(), idle_json);
        fields.insert("release_busy".to_string(), busy_json);
        fields.insert("receipt".to_string(), receipt_json);
        fields.insert("person_lane".to_string(), person_json);
        fields.insert("escalated_mix".to_string(), mix_json);
    }
    drop(ruler);
    (json, md)
}

/// Every leg's raw numbers.
struct Legs {
    idle: Idle,
    busy: Busy,
    rounds: Vec<Round>,
    rounds_spent: Spent,
    receipts: Vec<Receipt>,
    receipts_spent: Spent,
    persons: Person,
    mix: Mix,
    /// The asked and deadline tables' rows as each leg began, and at the end.
    tables: Vec<(&'static str, (usize, usize))>,
}

/// What every report reads.
struct Reading<'a> {
    heads: &'a Heads,
    /// The strict yardstick's p50: the unit.
    unit: Option<f64>,
    /// [`IDLE_ACK_GRACE`] plus one barrier, in ms.
    threshold: f64,
}

impl Reading<'_> {
    fn acked(&self, off: u64) -> Option<u64> {
        self.heads.acked.landed(off)
    }

    fn durable(&self, off: u64) -> Option<u64> {
        self.heads.durable.landed(off)
    }

    /// A post's release, split at the broker's heads: `(call -> released,
    /// call -> acked, acked -> released, call -> durable)`.
    fn split(&self, w: &Waited) -> Split {
        let acked = self.acked(w.answer.off);
        Split {
            released: ms(w.t_call, w.t_ret),
            acked: ms_opt(Some(w.t_call), acked),
            acked_to_released: ms_opt(acked, Some(w.t_ret)),
            durable: ms_opt(Some(w.t_call), self.durable(w.answer.off)),
        }
    }
}

/// [`Reading::split`].
struct Split {
    released: f64,
    acked: Option<f64>,
    acked_to_released: Option<f64>,
    durable: Option<f64>,
}

/// The four series of [`Split`]s.
#[derive(Default)]
struct Splits {
    released: Vec<f64>,
    acked: Vec<f64>,
    acked_to_released: Vec<f64>,
    durable: Vec<f64>,
}

impl Splits {
    fn push(&mut self, s: Split) {
        self.released.push(s.released);
        self.acked.extend(s.acked);
        self.acked_to_released.extend(s.acked_to_released);
        self.durable.extend(s.durable);
    }

    fn rows(&self, ctx: &Reading, what: &str) -> Vec<String> {
        vec![
            row_u(
                &format!("{what}: post call -> released"),
                &self.released,
                ctx.unit,
            ),
            row_u(
                &format!("{what}: post call -> acked"),
                &self.acked,
                ctx.unit,
            ),
            row_u(
                &format!("{what}: acked -> released"),
                &self.acked_to_released,
                ctx.unit,
            ),
            row_u(
                &format!("{what}: post call -> durable"),
                &self.durable,
                ctx.unit,
            ),
        ]
    }

    fn json(&self, ctx: &Reading) -> Value {
        json!({
            "post_call_to_released_ms": json_u(&self.released, ctx.unit),
            "post_call_to_acked_ms": json_u(&self.acked, ctx.unit),
            "acked_to_released_ms": json_u(&self.acked_to_released, ctx.unit),
            "post_call_to_durable_ms": json_u(&self.durable, ctx.unit),
            "late": late_json(late(&self.acked_to_released, ctx.threshold), ctx.threshold),
        })
    }
}

fn ms_of(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pooled_p50(before: &[f64], after: &[f64]) -> Option<f64> {
    let pooled: Vec<f64> = before.iter().chain(after).copied().collect();
    stats::summarize(&pooled).map(|s| s.p50)
}

fn idle_report(
    ctx: &Reading,
    idle: &Idle,
    recs: &[census::Rec],
    group: &str,
) -> (Value, Vec<String>) {
    let mut md = vec![
        String::new(),
        "### An ask's release, idle: `kind=ask` against `kind=note --wait`\n".to_string(),
        format!(
            "{} of each kind from A to B, one at a time, in blocks note, ask, ask, note; B lists \
             each. The note is the control: the same post, the same wait, without the ask's own \
             writes.\n",
            idle.releases[0].len()
        ),
        HEAD_U[0].to_string(),
        HEAD_U[1].to_string(),
    ];
    let mut json = aterm_json::Map::new();
    let mut lates = Vec::new();
    let mut censuses = Vec::new();
    for (v, kind) in Kind::BOTH.iter().enumerate() {
        let mut splits = Splits::default();
        let mut visible = Vec::new();
        for r in &idle.releases[v] {
            splits.push(ctx.split(&r.waited));
            visible.push(ms(r.waited.t_ret, r.t_seen));
        }
        md.extend(splits.rows(ctx, kind.word()));
        md.push(row_u(
            &format!("{}: released -> visible to B", kind.word()),
            &visible,
            ctx.unit,
        ));
        lates.push(late(&splits.acked_to_released, ctx.threshold));
        let windows: Vec<(u64, u64)> = idle.windows[v]
            .iter()
            .map(|&(lo, last)| {
                (
                    lo,
                    census::covering_commit(recs, group, last, last).map_or(last, |r| r.seq),
                )
            })
            .collect();
        let c = census::census(
            windows
                .iter()
                .flat_map(|&(lo, hi)| census::window(recs, lo, hi)),
        );
        let n = idle.releases[v].len();
        let mut fields = splits.json(ctx);
        if let Some(f) = fields.as_object_mut() {
            f.insert(
                "released_to_visible_ms".to_string(),
                json_u(&visible, ctx.unit),
            );
            f.insert("steps".to_string(), steps_json(&idle.spent[v], n));
            f.insert(
                "census".to_string(),
                census::to_json(&c, n, windows.first().copied().unwrap_or((0, 0))),
            );
        }
        json.insert(kind.word().to_string(), fields);
        censuses.push((kind.word(), c, idle.spent[v].flushes, n));
    }
    md.push(String::new());
    md.push(format!(
        "**Late releases** (more than {:.3} ms after the ack): {} asks and {} notes.\n",
        ctx.threshold,
        share_text(lates[1]),
        share_text(lates[0])
    ));
    md.push(
        "Where the bridge's loop spent a post, by kind (the wait tally's rows over each kind's \
         blocks, per post, B's delivery and commit included):\n"
            .to_string(),
    );
    let (n_note, n_ask) = (idle.releases[0].len(), idle.releases[1].len());
    md.extend(steps_rows(&[
        ("note", &idle.spent[0], n_note),
        ("ask", &idle.spent[1], n_ask),
    ]));
    md.push(String::new());
    md.push(
        "Records the broker wrote per post, by kind (each block from its first post to the \
         commit covering its last), and the broker's flushes per post:\n"
            .to_string(),
    );
    md.extend(census_rows(&censuses));
    md.push(String::new());
    (Value::Object(json), md)
}

/// Records per post by kind for several columns (`(name, census, flushes,
/// posts)`), then the totals and the broker's flushes per post.
fn census_rows(columns: &[(&str, census::Census, FlushCounts, usize)]) -> Vec<String> {
    let mut head = "| record kind |".to_string();
    let mut rule = "|---|".to_string();
    for (name, _, _, _) in columns {
        head.push_str(&format!(" {name}: per post |"));
        rule.push_str("---:|");
    }
    let mut rows = vec![head, rule];
    let classes: std::collections::BTreeSet<&String> = columns
        .iter()
        .flat_map(|(_, c, _, _)| c.by_class.keys())
        .collect();
    let line = |what: &str, value: &dyn Fn(&census::Census, FlushCounts) -> u64| {
        let mut l = format!("| {what} |");
        for (_, c, f, n) in columns {
            l.push_str(&format!(" {} |", fmt3(per(value(c, *f) as f64, *n))));
        }
        l
    };
    for class in classes {
        rows.push(line(&format!("`{class}`"), &|c, _| {
            c.by_class.get(class).copied().unwrap_or(0) as u64
        }));
    }
    rows.push(line("**all records**", &|c, _| c.total as u64));
    rows.push(line("of which commit records", &|c, _| c.commits as u64));
    rows.push(line("broker barriers (`F_BARRIERFSYNC`)", &|_, f| {
        f.barrier
    }));
    rows.push(line("**broker full flushes (`F_FULLFSYNC`)**", &|_, f| {
        f.full
    }));
    rows
}

fn busy_report(ctx: &Reading, legs: &Legs) -> (Value, Vec<String>) {
    let busy = &legs.busy;
    let mut senders = Splits::default();
    let mut visible = Vec::new();
    for (j, waited) in &busy.posted {
        senders.push(ctx.split(waited));
        if let Some(&t) = busy.seen.get(*j) {
            visible.push(ms(waited.t_call, t));
        }
    }
    let mut pings = Splits::default();
    let mut pongs = Splits::default();
    let mut round_trip = Vec::new();
    let mut ask_visible = Vec::new();
    for r in &legs.rounds {
        pings.push(ctx.split(&r.ping));
        pongs.push(ctx.split(&r.pong));
        round_trip.push(ms(r.ping.t_call, r.t_a_seen));
        ask_visible.push(ms(r.ping.t_call, r.t_b_seen));
    }
    let n = busy.posted.len();
    let mut md = vec![
        String::new(),
        "### An ask's release under load\n".to_string(),
        format!(
            "THE SENDERS: {BUSY_SENDERS} sessions post {n} asks to B at once, each waiting for \
             its release before its next, while a reader lists B's inbox. THE PING-PONG: {} \
             rounds back to back, A asks B and B, the moment it lists the ask, posts a `kind=task` \
             back (which waits too, and reaches A as a note: the world accepts tasks from no \
             node); nothing waits for the bridge's commit between them. Idle, the loop waits for \
             a waited post's ack before it parks; busy (anything queued for it: a record, an \
             event, a closure), it does not, and the ack is read by the first blocking settle \
             that comes: the next outbox drain (which settles every post owed before its \
             peek), a broker read (`conn_now` settles what is owed first), a send made outside a \
             round, or a round whose park, at most the idle ack grace ({} ms), took nothing. The \
             non-blocking pass (`try_settle_acks`) runs only on an idle loop.\n",
            legs.rounds.len(),
            ms_of(IDLE_ACK_GRACE)
        ),
        HEAD_U[0].to_string(),
        HEAD_U[1].to_string(),
    ];
    md.extend(senders.rows(ctx, "senders"));
    md.push(row_u(
        "senders: post call -> visible to B",
        &visible,
        ctx.unit,
    ));
    md.push(row_u(
        "ping-pong: **round trip, ask and task**",
        &round_trip,
        ctx.unit,
    ));
    md.push(row_u(
        "ping-pong: post call -> the ask visible to B",
        &ask_visible,
        ctx.unit,
    ));
    md.extend(pings.rows(ctx, "ping-pong, the ask"));
    md.extend(pongs.rows(ctx, "ping-pong, the task"));
    md.push(String::new());
    md.push(format!(
        "**Late releases** (more than {:.3} ms after the ack): the senders' {}; the ping-pong's \
         asks {} and tasks {}.\n",
        ctx.threshold,
        share_text(late(&senders.acked_to_released, ctx.threshold)),
        share_text(late(&pings.acked_to_released, ctx.threshold)),
        share_text(late(&pongs.acked_to_released, ctx.threshold)),
    ));
    md.push(
        "Where the bridge's loop spent a post (the ping-pong's two posts a round):\n".to_string(),
    );
    md.extend(steps_rows(&[
        ("senders", &busy.spent, n),
        ("ping-pong", &legs.rounds_spent, 2 * legs.rounds.len()),
    ]));
    md.push(String::new());
    md.push(flushes_per_post(
        "The senders' broker flushes",
        busy.spent.flushes,
        n,
    ));
    md.push(String::new());
    md.push(flushes_per_post(
        "The ping-pong's broker flushes",
        legs.rounds_spent.flushes,
        2 * legs.rounds.len(),
    ));
    md.push(String::new());
    let json = json!({
        "senders": {
            "sessions": BUSY_SENDERS,
            "asks": n,
            "split": senders.json(ctx),
            "post_call_to_visible_ms": json_u(&visible, ctx.unit),
            "steps": steps_json(&busy.spent, n),
        },
        "pingpong": {
            "rounds": legs.rounds.len(),
            "round_trip_ms": json_u(&round_trip, ctx.unit),
            "post_call_to_ask_visible_ms": json_u(&ask_visible, ctx.unit),
            "ask": pings.json(ctx),
            "task": pongs.json(ctx),
            "steps": steps_json(&legs.rounds_spent, 2 * legs.rounds.len()),
        },
    });
    (json, md)
}

fn receipt_report(ctx: &Reading, receipts: &[Receipt], spent: &Spent) -> (Value, Vec<String>) {
    let mut total = Vec::new();
    let mut to_acked = Vec::new();
    let mut acked_to_b = Vec::new();
    let mut turnaround = Vec::new();
    let mut verdict_call = Vec::new();
    let mut verdict_to_ack = Vec::new();
    let mut ack_to_released = Vec::new();
    for r in receipts {
        let acked = ctx.acked(r.waited.answer.off);
        let ack_acked = ctx.acked(r.ack_off);
        total.push(ms(r.waited.t_call, r.waited.t_ret));
        to_acked.extend(ms_opt(Some(r.waited.t_call), acked));
        acked_to_b.extend(ms_opt(acked, Some(r.t_b_seen)));
        turnaround.push(ms(r.t_b_seen, r.t_verdict));
        verdict_call.push(ms(r.t_verdict, r.t_verdict_ok));
        verdict_to_ack.extend(ms_opt(Some(r.t_verdict), ack_acked));
        ack_to_released.extend(ms_opt(ack_acked, Some(r.waited.t_ret)));
    }
    let n = receipts.len();
    let mut md = vec![
        String::new(),
        "### A receipt: `post kind=ask --wait-ack`\n".to_string(),
        format!(
            "{n} rounds, one at a time: A posts an ask with `--wait-ack`, which parks until B's \
             verdict comes back; B, on a second connection, wakes on it, lists it and answers \
             `inbox seen <id> handled`; its bridge publishes the `kind=ack`, and A's post answers \
             `OK <id> off=<n> ack=handled`. The landing (the ask acked) and the receipt (the ack's \
             record acked) are timed apart.\n"
        ),
        HEAD_U[0].to_string(),
        HEAD_U[1].to_string(),
        row_u("**post call -> `ack=handled`**", &total, ctx.unit),
        row_u(
            "post call -> the ask acked (its landing)",
            &to_acked,
            ctx.unit,
        ),
        row_u("the ask acked -> visible to B", &acked_to_b, ctx.unit),
        row_u(
            "B's turnaround (the bench: wake, list)",
            &turnaround,
            ctx.unit,
        ),
        row_u(
            "`inbox seen <id> handled` call -> `OK`",
            &verdict_call,
            ctx.unit,
        ),
        row_u(
            "`inbox seen` call -> the ack's record acked",
            &verdict_to_ack,
            ctx.unit,
        ),
        row_u("the ack acked -> A released", &ack_to_released, ctx.unit),
        String::new(),
        "Where the bridge's loop spent a round:\n".to_string(),
    ];
    md.extend(steps_rows(&[("receipt", spent, n)]));
    md.push(String::new());
    md.push(flushes_per_post(
        "Broker flushes over the rounds",
        spent.flushes,
        n,
    ));
    md.push(String::new());
    let json = json!({
        "rounds": n,
        "post_call_to_ack_handled_ms": json_u(&total, ctx.unit),
        "post_call_to_ask_acked_ms": json_u(&to_acked, ctx.unit),
        "ask_acked_to_visible_to_b_ms": json_u(&acked_to_b, ctx.unit),
        "b_turnaround_ms": json_u(&turnaround, ctx.unit),
        "verdict_call_ms": json_u(&verdict_call, ctx.unit),
        "verdict_call_to_ack_acked_ms": json_u(&verdict_to_ack, ctx.unit),
        "ack_acked_to_released_ms": json_u(&ack_to_released, ctx.unit),
        "steps": steps_json(spent, n),
    });
    (json, md)
}

fn person_report(ctx: &Reading, p: &Person) -> (Value, Vec<String>) {
    let (n0, n1) = (p.asks[0].len(), p.asks[1].len());
    let pace_ms = u64::try_from(PERSON_PACE.as_millis()).unwrap_or(u64::MAX);
    let mut md = vec![
        String::new(),
        format!("### A person's ask: `to={PERSON}`, escalated, without and with its pager\n"),
        format!(
            "Asks from A to the person `{PERSON}`, one at a time and at least {pace_ms} ms apart \
             call to call, {n0} without a pager and {n1} with one, in blocks unpaged, paged, \
             paged, unpaged. aterm's table escalates the person lane (`/f/*/in/p/>`), so the \
             broker flushes each ask in full before its ack: acked is durable. In the paged \
             blocks an `aterm link notify --on ask:{PERSON} --exec true` on this machine follows \
             the lane: once an ask is durable it writes its rate stamps and its journal's `<off> \
             firing` line, runs the command, writes the `<off> fired` line and then its cursor, \
             each a file replaced and flushed in full (`F_FULLFSYNC`) on the volume the bridge's \
             state dir is on. \"Paged\" is the `fired` line seen by the bench; the bench waits \
             for the cursor too before the next ask, so no ask is posted while the pager still \
             writes for the one before. The unpaged variant is the escalation's own cost; what \
             the paged one adds is the pager's.\n"
        ),
        HEAD_U[0].to_string(),
        HEAD_U[1].to_string(),
    ];
    let mut json = aterm_json::Map::new();
    let mut lates = Vec::new();
    for (v, word) in PERSON_ARMS.iter().enumerate() {
        let asks = &p.asks[v];
        let mut splits = Splits::default();
        let mut acked_to_durable = Vec::new();
        let mut gaps = Vec::new();
        let mut paged = Vec::new();
        let mut released_to_paged = Vec::new();
        let mut paged_to_quiet = Vec::new();
        for a in asks {
            splits.push(ctx.split(&a.waited));
            acked_to_durable.extend(ms_opt(
                ctx.acked(a.waited.answer.off),
                ctx.durable(a.waited.answer.off),
            ));
            gaps.extend(a.gap);
            if let Some((t_paged, t_quiet)) = a.pager {
                paged.push(ms(a.waited.t_call, t_paged));
                released_to_paged.push(ms(a.waited.t_ret, t_paged));
                paged_to_quiet.push(ms(t_paged, t_quiet));
            }
        }
        md.extend(splits.rows(ctx, word));
        md.push(row_u(
            &format!("{word}: acked -> durable"),
            &acked_to_durable,
            ctx.unit,
        ));
        md.push(row_u(
            &format!("{word}: call -> the next ask's call"),
            &gaps,
            ctx.unit,
        ));
        if !paged.is_empty() {
            md.push(row_u(
                &format!("{word}: **post call -> paged**"),
                &paged,
                ctx.unit,
            ));
            md.push(row_u(
                &format!("{word}: released -> paged"),
                &released_to_paged,
                ctx.unit,
            ));
            md.push(row_u(
                &format!("{word}: paged -> the pager's cursor written"),
                &paged_to_quiet,
                ctx.unit,
            ));
        }
        lates.push(late(&splits.acked_to_released, ctx.threshold));
        let mut fields = splits.json(ctx);
        if let Some(f) = fields.as_object_mut() {
            f.insert("asks".to_string(), json!(asks.len()));
            f.insert(
                "acked_to_durable_ms".to_string(),
                json_u(&acked_to_durable, ctx.unit),
            );
            f.insert("call_to_next_call_ms".to_string(), json_u(&gaps, ctx.unit));
            if !paged.is_empty() {
                f.insert(
                    "post_call_to_paged_ms".to_string(),
                    json_u(&paged, ctx.unit),
                );
                f.insert(
                    "released_to_paged_ms".to_string(),
                    json_u(&released_to_paged, ctx.unit),
                );
                f.insert(
                    "paged_to_cursor_ms".to_string(),
                    json_u(&paged_to_quiet, ctx.unit),
                );
            }
            f.insert("steps".to_string(), steps_json(&p.spent[v], asks.len()));
        }
        json.insert((*word).to_string(), fields);
    }
    md.push(String::new());
    md.push(format!(
        "**Late releases** (more than {:.3} ms after the ack): {} unpaged and {} paged.\n",
        ctx.threshold,
        share_text(lates[0]),
        share_text(lates[1])
    ));
    for (v, word) in PERSON_ARMS.iter().enumerate() {
        md.push(flushes_per_post(
            &format!("Broker flushes over the {word} asks"),
            p.spent[v].flushes,
            p.asks[v].len(),
        ));
        md.push(String::new());
    }
    md.push(
        "Where the bridge's loop spent an ask, without and with the pager (the pager's own \
         writes are not the bridge's, so no row counts them; among the state dir's replaces \
         are the bridge's asked and deadline tables, each rewritten whole by every ask):\n"
            .to_string(),
    );
    md.extend(steps_rows(&[
        (PERSON_ARMS[0], &p.spent[0], n0),
        (PERSON_ARMS[1], &p.spent[1], n1),
    ]));
    md.push(String::new());
    json.insert("pace_ms".to_string(), json!(pace_ms));
    (Value::Object(json), md)
}

fn mix_report(ctx: &Reading, spec: &Spec, m: &Mix) -> (Value, Vec<String>) {
    let series = |msgs: &[Plain], pick: &dyn Fn(&Plain) -> bool| {
        let mut visible = Vec::new();
        let mut end_to_end = Vec::new();
        let mut acked = Vec::new();
        for p in msgs.iter().filter(|p| pick(p)) {
            visible.push(ms(p.t_ret, p.t_seen));
            end_to_end.push(ms(p.t_call, p.t_seen));
            acked.extend(ms_opt(Some(p.t_ret), ctx.acked(p.off)));
        }
        (visible, end_to_end, acked)
    };
    let plain = series(&m.messages[0], &|_| true);
    let mixed = series(&m.messages[1], &|_| true);
    let after = series(&m.messages[1], &|p| p.after_person);
    let others = series(&m.messages[1], &|p| !p.after_person);
    let (n0, n1) = (m.messages[0].len(), m.messages[1].len());
    let mut md = vec![
        String::new(),
        format!(
            "### The person lane's bystander cost: one person-lane post every {} messages\n",
            spec.mix_every
        ),
        format!(
            "The messaging phase's latency loop (A posts B a `kind=note`, B lists it, the next \
             waits for the commit), {n0} messages without and {n1} with a person-lane post by a \
             third session C (`to={PERSON} kind=note`, answered when queued) just before one \
             message in every {}, in blocks plain, mixed, mixed, plain. The broker flushes C's \
             record in full \
             before its ack, and every barrier on the volume waits behind that flush.\n",
            spec.mix_every
        ),
        HEAD_U[0].to_string(),
        HEAD_U[1].to_string(),
        row_u("plain: **post -> visible to B**", &plain.0, ctx.unit),
        row_u("mixed: **post -> visible to B**", &mixed.0, ctx.unit),
        row_u(
            "mixed, just after a person-lane post: post -> visible",
            &after.0,
            ctx.unit,
        ),
        row_u("mixed, the others: post -> visible", &others.0, ctx.unit),
        row_u("plain: post call -> visible", &plain.1, ctx.unit),
        row_u("mixed: post call -> visible", &mixed.1, ctx.unit),
        row_u("plain: post -> acked", &plain.2, ctx.unit),
        row_u("mixed: post -> acked", &mixed.2, ctx.unit),
        row_u(
            "mixed, just after a person-lane post: post -> acked",
            &after.2,
            ctx.unit,
        ),
        String::new(),
        flushes_per_post(
            "Broker flushes per ordinary message, plain",
            m.spent[0].flushes,
            n0,
        ),
        String::new(),
        flushes_per_post(
            &format!(
                "Broker flushes per ordinary message, mixed ({} person-lane posts among them)",
                m.person_posts
            ),
            m.spent[1].flushes,
            n1,
        ),
        String::new(),
    ];
    md.push("Where the bridge's loop spent a message:\n".to_string());
    md.extend(steps_rows(&[
        ("plain", &m.spent[0], n0),
        ("mixed", &m.spent[1], n1),
    ]));
    md.push(String::new());
    let part = |s: &(Vec<f64>, Vec<f64>, Vec<f64>)| {
        json!({
            "post_to_visible_ms": json_u(&s.0, ctx.unit),
            "post_call_to_visible_ms": json_u(&s.1, ctx.unit),
            "post_to_acked_ms": json_u(&s.2, ctx.unit),
        })
    };
    let json = json!({
        "mix_every": spec.mix_every,
        "person_posts": m.person_posts,
        "plain": { "messages": n0, "series": part(&plain), "steps": steps_json(&m.spent[0], n0) },
        "mixed": { "messages": n1, "series": part(&mixed), "steps": steps_json(&m.spent[1], n1) },
        "mixed_after_person": part(&after),
        "mixed_others": part(&others),
    });
    (json, md)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_waiting_posts_answer_reads_its_offset_and_its_verdict() {
        use super::*;

        assert_eq!(
            answer_of("OK 7 off=42"),
            Some(Answer {
                id: 7,
                off: 42,
                ack: None,
                msg: None
            })
        );
        assert_eq!(
            answer_of("OK 3 off=9 dup=1 ack=handled msg=11"),
            Some(Answer {
                id: 3,
                off: 9,
                ack: Some("handled".to_string()),
                msg: Some(11)
            })
        );
        for bad in ["OK 7", "ERR timeout id=7", "OK x off=1", "OK 1 off=x", ""] {
            assert_eq!(answer_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_blocks_run_forward_then_back_and_give_each_variant_its_posts() {
        use super::*;

        assert_eq!(blocks(5, 2), vec![(0, 3), (1, 3), (1, 2), (0, 2)]);
        assert_eq!(blocks(1, 2), vec![(0, 1), (1, 1)]);
        assert_eq!(
            blocks(4, 3),
            vec![(0, 2), (1, 2), (2, 2), (2, 2), (1, 2), (0, 2)]
        );
        for (n, arms) in [(1, 2), (7, 2), (200, 3), (9, 1)] {
            let each: Vec<usize> = (0..arms)
                .map(|v| {
                    blocks(n, arms)
                        .iter()
                        .filter(|&&(w, _)| w == v)
                        .map(|&(_, k)| k)
                        .sum()
                })
                .collect();
            assert!(each.iter().all(|&k| k == n), "{n} {arms}: {each:?}");
        }
    }

    #[test]
    fn a_tally_is_settled_past_the_commit_and_the_retired_posts() {
        use super::*;

        let mut t = Tally {
            pid: 1,
            committed: 40,
            ..Tally::default()
        };
        t.rows[TallyRow::LaneSent.index()].count = 5;
        assert!(settled(40, 5)(&t).is_ok());
        assert!(settled(0, 0)(&t).is_ok());
        assert!(settled(41, 5)(&t).expect_err("short of 41").contains("41"));
        assert!(
            settled(40, 6)(&t)
                .expect_err("five retired")
                .contains("retired 5")
        );
    }

    #[test]
    fn the_windows_add_up_and_the_steps_read_per_post() {
        use super::*;

        let mut d = Tally {
            pid: 1,
            committed: 9,
            dir: true,
            at_ns: 4_000_000,
            ..Tally::default()
        };
        d.rows[TallyRow::DirReplace.index()].count = 4;
        d.rows[TallyRow::DirReplace.index()].total = 2_000_000;
        let mut s = Spent::default();
        let f = FlushCounts {
            barrier: 3,
            full: 1,
            skipped: 0,
            failed: 0,
        };
        s.add_delta(&d, f);
        s.add_delta(&d, f);
        assert_eq!(
            (s.count(TallyRow::DirReplace), s.ns(TallyRow::DirReplace)),
            (8, 4_000_000)
        );
        assert_eq!(
            (s.at_ns, s.windows, s.flushes.barrier, s.flushes.full),
            (8_000_000, 2, 6, 2)
        );
        assert!(s.dir);
        let mut note = Spent::default();
        note.add_delta(&Tally { dir: true, ..d }, f);
        let rows = steps_rows(&[("note", &note, 2), ("ask", &s, 2)]);
        let replace = rows
            .iter()
            .find(|r| r.starts_with(&format!("| {} |", describe(TallyRow::DirReplace))))
            .expect("the replace row");
        assert_eq!(
            replace,
            &format!(
                "| {} | yes | 2.000 | 1.000 | 4.000 | 2.000 | +1.000 |",
                describe(TallyRow::DirReplace)
            )
        );
        assert!(
            rows.iter()
                .all(|r| !r.contains(describe(TallyRow::Request))),
            "a row neither column counted is left out"
        );
        let undir = Spent { dir: false, ..note };
        assert!(
            steps_rows(&[("x", &undir, 1)])
                .last()
                .is_some_and(|r| r.contains("not counted"))
        );
        assert_eq!(
            steps_json(&s, 2)["rows"]["dir_replace"]["count_per_post"],
            json!(4.0)
        );
    }

    #[test]
    fn the_census_reads_each_kind_per_post_beside_the_flushes() {
        use super::*;

        let census = |classes: &[(&str, usize)], commits: usize| census::Census {
            total: classes.iter().map(|(_, n)| n).sum(),
            hidden: commits,
            commits,
            by_class: classes
                .iter()
                .map(|(c, n)| ((*c).to_string(), *n))
                .collect(),
        };
        let note = census(&[("/a/commit", 4), ("note", 4)], 4);
        let ask = census(&[("/a/commit", 4), ("ask", 4)], 4);
        let flushes = |full| FlushCounts {
            barrier: 8,
            full,
            skipped: 0,
            failed: 0,
        };
        let rows = census_rows(&[("note", note, flushes(0), 4), ("ask", ask, flushes(2), 4)]);
        assert_eq!(rows[0], "| record kind | note: per post | ask: per post |");
        assert!(
            rows.contains(&"| `ask` | 0.000 | 1.000 |".to_string()),
            "{rows:?}"
        );
        assert!(
            rows.contains(&"| `note` | 1.000 | 0.000 |".to_string()),
            "{rows:?}"
        );
        assert!(rows.contains(&"| **all records** | 2.000 | 2.000 |".to_string()));
        assert_eq!(
            rows.last().map(String::as_str),
            Some("| **broker full flushes (`F_FULLFSYNC`)** | 0.000 | 0.500 |")
        );
    }

    #[test]
    fn the_tables_read_before_each_leg_and_at_the_end() {
        use super::*;

        assert_eq!(
            tables_text(&[
                ("the idle leg", (0, 0)),
                ("the receipts", (300, 200)),
                ("the end", (612, 512))
            ]),
            "0/0 before the idle leg, 300/200 before the receipts, 612/512 at the end"
        );
    }

    #[test]
    fn the_tables_must_grow_by_the_unanswered_asks_up_to_their_caps() {
        use super::*;
        use aterm_link::state::{ASKED_KEEP, DEADLINES_KEEP};

        assert_eq!(tables_grew((0, 0), (103, 103), 100), Ok(()));
        assert_eq!(
            tables_grew(
                (ASKED_KEEP - 10, DEADLINES_KEEP - 10),
                (ASKED_KEEP, DEADLINES_KEEP),
                100
            ),
            Ok(())
        );
        let moved = tables_grew((0, 0), (0, 0), 100).expect_err("a table read where none is");
        assert!(moved.contains("0/0 after 100 unanswered asks"), "{moved}");
        assert!(tables_grew((0, 0), (100, 0), 100).is_err());
        assert!(tables_grew((5, 5), (104, 105), 100).is_err());
    }

    #[test]
    fn a_release_is_late_only_past_the_threshold() {
        use super::*;

        assert_eq!(late(&[1.0, 5.9, 6.0, 6.1, 20.0], 6.0), (2, 5));
        assert_eq!(late(&[], 6.0), (0, 0));
        assert_eq!(share_text((2, 5)), "2 of 5 (40.0%)");
        assert_eq!(share_text((0, 0)), "–");
        assert_eq!(
            row_u("x", &[2.0, 4.0], Some(2.0)),
            "| x | 2 | 3.00 | 2.00 | 4.00 | 4.00 | 1.00 |"
        );
        assert_eq!(
            json_u(&[2.0, 4.0], Some(2.0))["p50_in_publishes"],
            json!(1.0)
        );
    }
}
