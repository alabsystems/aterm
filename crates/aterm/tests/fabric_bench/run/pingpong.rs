// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ping-pong arm: back-to-back request and reply on one node.

use std::path::Path;
use std::time::Duration;

use astream_broker::{Broker, FlushCounts};
use aterm_json::{Value, json};
use aterm_link::ctl::Ctl;
use aterm_link::wait_tally::{self, Flushed, Hist, Row as TallyRow, Tally, Unit};
use aterm_types::wait_tally::{HANDLER_FILE, HANDLER_SEAM, HandlerRow, HandlerTally};

use super::tallies::{self, Mark, SlowRound};
use super::{
    DEADLINE, FLEET, Row, Ruler, Ticks, World, await_durable, await_note, boot_world, bystanders,
    calibrate, flushes_between, flushes_json, fmt_x, listing_since, ms, ms_opt, newest_id,
    per_unit, post, rows_of, sampled, served_line, stop_and_read, ticks, ticks_between, ticks_json,
    ticks_line, wait_live, yardstick, yardstick_json, yardstick_rows,
};
use crate::census;
use crate::clock::Clock;
use crate::spec::Spec;
use crate::stats;

const TALLY_POLL: Duration = Duration::from_millis(1);

pub(super) const PACE: Duration = Duration::from_millis(20);

/// One round, as the bench saw it.
#[derive(Debug, Clone)]
pub(super) struct Round {
    ping: String,
    pong: String,
    /// A's post: before the request, after its `OK`.
    pub(super) t_call: u64,
    t_ret: u64,
    /// B's `await inbox` answering with the ping, and its `inbox` listing it.
    pub(super) t_b_seen: u64,
    t_b_listed: u64,
    /// B's post of the reply: before the request, after its `OK`.
    t_b_call: u64,
    pub(super) t_b_ret: u64,
    /// A's `await inbox` answering with the reply, and its `inbox` listing it.
    pub(super) t_a_seen: u64,
    t_a_listed: u64,
    /// The rows' `off=`: each message's record offset.
    ping_off: u64,
    pong_off: u64,
}

struct Raw {
    rounds: Vec<Round>,
    /// The tally past the warm-up, and the one past the last reply.
    start: Tally,
    end: Tally,
    /// aterm's own handler tally ([`handler_tally`]) at the same two moments.
    handlers_start: HandlerTally,
    handlers_end: HandlerTally,
    /// The broker's flushes between those two tallies
    /// ([`Broker::flush_counts`], read right after each: between two round
    /// trips, never inside one).
    flushes: FlushCounts,
    /// The broker's retention ticks between the same two tallies ([`Ticks`]).
    ticks: Ticks,
    /// THE PACED SUB-PHASE, after the back-to-back rounds' last tally
    /// ([`Spec::paced`], [`PACE`] apart): its rounds, and the tally past its
    /// last reply (its window runs from `end`).
    paced: Vec<Round>,
    paced_end: Option<Tally>,
    /// The handler tally past the last paced reply (its window runs from `handlers_end`).
    paced_handlers_end: Option<HandlerTally>,
    /// This process's flush times and the clock after them, read after each of the three wait
    /// tallies (`start`, `end`, `paced_end`): its broker's ring and the bench's side of each
    /// reading's mark ([`bench_now`]).
    start_bench: (Flushed, u64),
    end_bench: (Flushed, u64),
    paced_bench: Option<(Flushed, u64)>,
}

/// The group ping-pong's slow calls are reported under.
const SLOW_GROUP: &str = "ping-pong";

/// This process's flush times now, then the clock: its in-process broker's ring and the
/// bench's side of a reading's [`Mark`], read after the bridge's line (E09c).
fn bench_now() -> (Flushed, u64) {
    let flushed = Flushed::now();
    (flushed, wait_tally::clock_ns())
}

/// The rounds whose slow calls the report lines up with the flushes under them ([`slow_rounds_of`]
/// of `raw`'s readings).
fn slow_rounds(raw: &Raw) -> Vec<SlowRound> {
    slow_rounds_of(
        (&raw.start, raw.start_bench),
        (&raw.end, raw.end_bench),
        raw.paced_end.as_ref().zip(raw.paced_bench),
    )
}

/// The rounds whose slow calls the report lines up with the flushes under them, from the
/// readings `start` (the bridge's tally past the warm-up and this process's flush times and
/// clock after it), `end` (past the last reply) and, when the paced sub-phase ran, `paced` (past
/// its last reply): the back-to-back rounds, from `start` to `end`, judged by `start`'s mark,
/// and the paced ones, from `end` to `paced`, by both (a call that began before `end` is judged
/// from `start`'s).
///
/// # Panics
///
/// Two of the bridge's tallies that do not pair (a relaunch, a count that went backwards).
fn slow_rounds_of(
    start: (&Tally, (Flushed, u64)),
    end: (&Tally, (Flushed, u64)),
    paced: Option<(&Tally, (Flushed, u64))>,
) -> Vec<SlowRound> {
    let mark = |t: &Tally, (bench, at): (Flushed, u64)| Mark::of(t, &bench, at);
    let round = |name: &str,
                 (earlier, from): (&Tally, (Flushed, u64)),
                 (later, to): (&Tally, (Flushed, u64)),
                 marks: Vec<Mark>| SlowRound {
        group: SLOW_GROUP.to_string(),
        round: name.to_string(),
        calls: later
            .since(earlier)
            .unwrap_or_else(|e| panic!("ping-pong, {name}: the wait tallies do not pair: {e}"))
            .calls,
        bridge_pid: later.pid,
        bridge: later.times,
        bench: to.0,
        window: (from.1, to.1),
        marks,
    };
    let opened = mark(start.0, start.1);
    let mut out = vec![round("back-to-back", start, end, vec![opened])];
    if let Some(paced) = paced {
        out.push(round("paced", end, paced, vec![opened, mark(end.0, end.1)]));
    }
    out
}

pub(super) fn one_round(
    ctl: &mut Ctl,
    clock: Clock,
    a: &str,
    b: &str,
    i: usize,
    a_since: &mut u64,
    b_since: &mut u64,
) -> Round {
    let ping = format!("ping-{i}");
    let pong = format!("pong-{i}");
    let (t_call, t_ret) = post(ctl, clock, a, b, &ping);
    let (ping_row, t_b_seen, t_b_listed) = woke_with(ctl, clock, b, b_since, &ping);
    let (t_b_call, t_b_ret) = post(ctl, clock, b, a, &pong);
    let (pong_row, t_a_seen, t_a_listed) = woke_with(ctl, clock, a, a_since, &pong);
    Round {
        ping,
        pong,
        t_call,
        t_ret,
        t_b_seen,
        t_b_listed,
        t_b_call,
        t_b_ret,
        t_a_seen,
        t_a_listed,
        ping_off: ping_row.off,
        pong_off: pong_row.off,
    }
}

fn covering(text: Option<&str>, pid: i32, off: u64) -> Result<Tally, String> {
    let tally = Tally::parse(text.ok_or("no tally written yet")?)?;
    if i64::from(tally.pid) != i64::from(pid) {
        return Err(format!(
            "the tally is pid {}'s, not the bridge's {pid}",
            tally.pid
        ));
    }
    if tally.committed < off {
        return Err(format!(
            "the tally's cursor is at {}, not yet through {off}",
            tally.committed
        ));
    }
    Ok(tally)
}

/// Wait for the bridge `pid`'s tally at `path` to have committed through
/// `off`: its idle write after the commit that covers it.
///
/// # Panics
///
/// After [`DEADLINE`] — the hang detector — with why the last read was not it.
fn await_tally(path: &Path, pid: i32, off: u64, what: &str) -> Tally {
    let mut last = String::new();
    aterm_spec::wait::within(DEADLINE, TALLY_POLL, || {
        let text = std::fs::read_to_string(path).ok();
        covering(text.as_deref(), pid, off)
            .map_err(|why| last = why)
            .ok()
    })
    .unwrap_or_else(|| {
        panic!(
            "{what}: the bridge wrote no wait tally through offset {off} at {} within \
             {DEADLINE:?}; the last read: {last}",
            path.display()
        )
    })
}

/// aterm's handler tally now: a wait with nothing to wait for has aterm write the line before it
/// answers ([`aterm_types::wait_tally::HANDLER_SEAM`]), so the file holds every span that ended
/// before this request, and the request itself counts in no row.
///
/// # Panics
///
/// A wait that answered anything but its timeout, or a line that is not this aterm's.
fn handler_tally(ctl: &mut Ctl, path: &Path, pid: u32, sid: &str, since: u64) -> HandlerTally {
    let line = format!("@{sid} await inbox since={since} kinds=note timeout=0");
    let reply = super::request(ctl, &line);
    assert_eq!(reply.header(), "OK timeout", "{line}");
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("aterm wrote no handler tally at {}: {e}", path.display()));
    let tally = HandlerTally::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(tally.pid, pid, "{}", path.display());
    tally
}

/// Park on `sid`'s inbox for the round's one message, `text`, and list it:
/// `(its row, the stamp when the wait answered, the stamp when the listing answered)`.
///
/// # Panics
///
/// A wait that timed out, or a wake that was not exactly `text`: a stray, or a
/// repeat, would be timed as the reply.
fn woke_with(
    ctl: &mut Ctl,
    clock: Clock,
    sid: &str,
    since: &mut u64,
    text: &str,
) -> (Row, u64, u64) {
    let (woke_at, t) =
        await_note(ctl, clock, sid, *since).unwrap_or_else(|e| panic!("`{text}`: {e}"));
    let reply = listing_since(ctl, sid, *since);
    let listed = clock.us();
    let rows = rows_of(&reply);
    assert!(
        rows.len() == 1 && rows[0].text == text && rows[0].id == woke_at,
        "`{text}`: @{sid} woke at row {woke_at} and listed {rows:?}"
    );
    *since = woke_at;
    (rows[0].clone(), t, listed)
}

fn rounds(w: &World, broker: &Broker, spec: &Spec, clock: Clock, a: &str, b: &str) -> Raw {
    let path = w.state.join(wait_tally::FILE);
    let pid = w.bridge_pid();
    let mut ctl = w.ctl();
    let mut a_since = newest_id(&mut ctl, a);
    let mut b_since = newest_id(&mut ctl, b);
    let handlers = w.instance_state().join(HANDLER_FILE);
    let gui = w.gui.as_ref().expect("the world's aterm").id();
    let mut start = if spec.warmup == 0 {
        Some((
            await_tally(&path, pid, 0, "ping-pong, before the first round"),
            broker.flush_counts(),
            ticks(broker),
            handler_tally(&mut ctl, &handlers, gui, a, a_since),
            bench_now(),
        ))
    } else {
        None
    };
    let mut measured = Vec::with_capacity(spec.pingpong);
    for i in 0..spec.warmup + spec.pingpong {
        let round = one_round(&mut ctl, clock, a, b, i, &mut a_since, &mut b_since);
        if i + 1 == spec.warmup {
            start = Some((
                await_tally(&path, pid, round.pong_off, "ping-pong, after the warm-up"),
                broker.flush_counts(),
                ticks(broker),
                handler_tally(&mut ctl, &handlers, gui, a, a_since),
                bench_now(),
            ));
        }
        if i >= spec.warmup {
            measured.push(round);
        }
    }
    let last = measured.last().expect("at least one measured round");
    let end = await_tally(&path, pid, last.pong_off, "ping-pong, after the last round");
    let end_bench = bench_now();
    let flushes_end = broker.flush_counts();
    let ticks_end = ticks(broker);
    let handlers_end = handler_tally(&mut ctl, &handlers, gui, a, a_since);
    let (start, flushes_start, ticks_start, handlers_start, start_bench) =
        start.expect("taken before the first measured round");
    let first = spec.warmup + spec.pingpong;
    let paced: Vec<Round> = (first..first + spec.paced)
        .map(|i| {
            std::thread::sleep(PACE);
            one_round(&mut ctl, clock, a, b, i, &mut a_since, &mut b_since)
        })
        .collect();
    let paced_end = paced.last().map(|last| {
        await_tally(
            &path,
            pid,
            last.pong_off,
            "ping-pong, after the last paced round",
        )
    });
    let paced_bench = paced_end.map(|_| bench_now());
    let paced_handlers_end = paced_end.map(|_| handler_tally(&mut ctl, &handlers, gui, a, a_since));
    Raw {
        rounds: measured,
        start,
        end,
        handlers_start,
        handlers_end,
        flushes: flushes_between(flushes_start, flushes_end),
        ticks: ticks_between(ticks_start, ticks_end),
        paced,
        paced_end,
        paced_handlers_end,
        start_bench,
        end_bench,
        paced_bench,
    }
}

/// Where an answer's record sits against the commit of the delivery it
/// answers, on the stopped log ([`behind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// The commit is before the answer's record: the bridge committed the
    /// delivery, then published the answer.
    After,
    /// The answer's record is before the commit, and the commit covers it as
    /// well: the bridge had delivered the answer too by the time it committed,
    /// so one commit covered both deliveries — a FOLD, one commit fewer than
    /// the two a round trip otherwise has.
    Folded,
    /// The answer's record is before the commit, and the commit's cursor stops
    /// short of it: the bridge published the answer ahead of the commit but
    /// committed only the delivery, so the answer's own delivery has a commit
    /// of its own, later.
    Ahead,
}

/// What the log says of one answer and the commit of the delivery it answers.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Behind {
    order: Order,
    /// The answer's wait for that commit, in ms: from its post `OK` to the
    /// commit acked, 0 when it was acked before the post.
    wait: Option<f64>,
}

fn behind(
    recs: &[census::Rec],
    group: &str,
    delivered: u64,
    answer: u64,
    answer_ok: u64,
    landed: impl Fn(u64) -> Option<u64>,
) -> Option<Behind> {
    let commit = census::covering_commit(recs, group, delivered, delivered)?;
    let order = if commit.seq < answer {
        Order::After
    } else if census::covering_commit(recs, group, answer, answer)
        .is_some_and(|c| c.seq == commit.seq)
    {
        Order::Folded
    } else {
        Order::Ahead
    };
    let wait = if order == Order::After {
        landed(commit.seq).map(|t| ms(answer_ok, t).max(0.0))
    } else {
        None
    };
    Some(Behind { order, wait })
}

/// One direction's answers ([`behind`]): how many sat in each [`Order`]
/// against the commit of the delivery they answer, and the waits of those
/// recorded after it.
#[derive(Debug, Default)]
struct Answers {
    after: usize,
    folded: usize,
    ahead: usize,
    waits: Vec<f64>,
}

impl Answers {
    fn add(&mut self, b: Behind) {
        match b.order {
            Order::After => self.after += 1,
            Order::Folded => self.folded += 1,
            Order::Ahead => self.ahead += 1,
        }
        self.waits.extend(b.wait);
    }
}

fn order_line(pongs: &Answers, pings: &Answers, n: usize) -> String {
    let mut line = format!(
        "The commit of the delivery a message answers landed before that message's own record \
         for {} of {n} pongs and {} of {} pings.",
        pongs.after,
        pings.after,
        n.saturating_sub(1)
    );
    if pongs.folded + pongs.ahead + pings.folded + pings.ahead > 0 {
        line.push_str(&format!(
            " Each of the others was recorded before that commit, so it never waited on it and \
             has no wait in the two rows above. Of those, {} pongs and {} pings were folds, the \
             commit covering their own delivery too (one commit for two deliveries, one commit \
             fewer), and {} pongs and {} pings were not, their own delivery having a later commit \
             of its own.",
            pongs.folded, pings.folded, pongs.ahead, pings.ahead
        ));
    }
    line.push('\n');
    line
}

fn share(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| stats::round3(part / whole))
}

pub(super) fn ns_ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn waits_rows(d: &Tally, n: usize) -> Vec<String> {
    let per = |v: f64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", v / n as f64)
        }
    };
    let row = |what: &str, count: Option<u64>, ns: u64| {
        let (count, count_per) = count.map_or(("–".to_string(), "–".to_string()), |c| {
            (c.to_string(), per(c as f64))
        });
        format!(
            "| {what} | {count} | {count_per} | {:.2} | {} |",
            ns_ms(ns),
            per(ns_ms(ns))
        )
    };
    let r = |row: TallyRow| d.row(row);
    vec![
        "| the loop waited on | count | per round trip | ms | ms per round trip |".to_string(),
        "|---|---:|---:|---:|---:|".to_string(),
        row(
            "requests that read their own reply",
            Some(r(TallyRow::Request).count),
            r(TallyRow::Request).total,
        ),
        row(
            "pipelined acks (publishes', and since aterm-0 the group commits')",
            Some(r(TallyRow::Ack).count),
            r(TallyRow::Ack).total,
        ),
        row(
            "of both: the inbox group's commits",
            Some(r(TallyRow::Commit).count),
            r(TallyRow::Commit).total,
        ),
        row("**all: the loop held by the broker**", None, d.blocked_ns()),
    ]
}

pub(super) const TAIL_1MS: usize = 11;
const TAIL_8MS: usize = 14;

pub(super) fn describe(row: TallyRow) -> &'static str {
    match row {
        TallyRow::Request => "broker: requests that read their own reply",
        TallyRow::Ack => "broker: pipelined acks read (publishes', the group commits')",
        TallyRow::Commit => "broker: of those, the inbox group's commits",
        TallyRow::LaneOutbox => "verb lane: the outbox peek",
        TallyRow::LaneSessions => "verb lane: a drain's roster read (`sessions bridge`)",
        TallyRow::LaneSent => "verb lane: a post's retirement (`outbox sent`)",
        TallyRow::LaneDeliver => "verb lane: deliveries (`deliver`)",
        TallyRow::LaneStatus => "verb lane: a session's status read (`@<sid> status`)",
        TallyRow::LaneMeta => "verb lane: a session's metadata read (`@<sid> meta`)",
        TallyRow::LaneText => "verb lane: a session's screen read (`@<sid> text`)",
        TallyRow::LaneSessionsStatus => {
            "verb lane: every session's status read at once (`sessions status`)"
        }
        TallyRow::LaneOther => "verb lane: every other verb",
        TallyRow::DirReplace => "state dir: replaces (temp + rename)",
        TallyRow::DirUnlink => "state dir: unlinks",
        TallyRow::DirAppend => "state dir: appends",
        TallyRow::DirFlush => "state dir: flushes, of a file in place or a directory",
        TallyRow::SetPostPin => "of the dir's: a post's reservation (`set_post_pin`)",
        TallyRow::SetInFlight => "of the dir's: a self-lane post's entry (`set_in_flight`)",
        TallyRow::ClearInFlight => "of the dir's: an entry's removal alone (`clear_in_flight`)",
        TallyRow::ClearPostSeq => {
            "of the dir's: posts and owed receipts settled (`settle_posts`, `clear_post_seq`)"
        }
        TallyRow::AppendSelfAcked => "of the dir's: the landed offset (`append_self_acked`)",
        TallyRow::LogpinFence => "of the dir's: the mirror pin's fence, once an open (`logpin`)",
        TallyRow::WakeEvent => "wake: an aterm event queued while the loop was parked",
        TallyRow::WakeRecord => "wake: a broker record queued while the loop was parked",
        TallyRow::QueuedEvent => "queued: an aterm event queued while the loop worked",
        TallyRow::QueuedRecord => "queued: a broker record queued while the loop worked",
        TallyRow::PostPublish => "a post's path: its `post` event taken to its publish written",
        TallyRow::DrainPosts => "posts each outbox peek listed (a count)",
        TallyRow::Park => "the loop parked on its mailbox",
        TallyRow::TallyWrite => "the tally's own idle writes",
    }
}

pub(super) fn largest(h: &Hist) -> String {
    if h.max > 0 {
        return format!("{:.3}", ns_ms(h.max));
    }
    match h.top(Unit::Ns) {
        None => "–".to_string(),
        Some((_, Some(bound))) => format!("< {:.3}", ns_ms(bound)),
        Some((floor, None)) => format!("≥ {:.3}", ns_ms(floor)),
    }
}

fn tally_rows(d: &Tally, n: usize) -> Vec<String> {
    let per = |v: f64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", v / n as f64)
        }
    };
    let mut rows = vec![
        format!(
            "| the loop's step (`{}`) | partitions | count | per round trip | ms | ms per round \
             trip | ≥ 1.05 ms | ≥ 8.39 ms | largest (ms) |",
            wait_tally::FORMAT
        ),
        "|---|---|---:|---:|---:|---:|---:|---:|---:|".to_string(),
    ];
    for row in TallyRow::ALL {
        if row.unit() != Unit::Ns {
            continue;
        }
        let h = d.row(row);
        rows.push(format!(
            "| {} | {} | {} | {} | {:.2} | {} | {} | {} | {} |",
            describe(row),
            if row.partitions() { "yes" } else { "" },
            h.count,
            per(h.count as f64),
            ns_ms(h.total),
            per(ns_ms(h.total)),
            h.at_least(TAIL_1MS),
            h.at_least(TAIL_8MS),
            largest(h),
        ));
    }
    let peeks = d.row(TallyRow::DrainPosts);
    rows.push(String::new());
    rows.push(format!(
        "Outbox peeks: {} ({} per round trip), listing {} posts; {} listed none.{}\n",
        peeks.count,
        per(peeks.count as f64),
        peeks.total,
        peeks.buckets[0],
        if d.dir {
            ""
        } else {
            " The state dir's rows were not counted: a build without its seams."
        }
    ));
    rows
}

fn attribution(d: &Tally, n: usize) -> Vec<(String, f64)> {
    if n == 0 {
        return Vec::new();
    }
    let per = |ns: u64| ns_ms(ns) / n as f64;
    let mut parts: Vec<(String, f64)> = TallyRow::ALL
        .iter()
        .filter(|r| r.partitions() && **r != TallyRow::Park)
        .map(|r| (describe(*r).to_string(), per(d.row(*r).total)))
        .collect();
    let park = d.row(TallyRow::Park).total;
    let rest = d.at_ns.saturating_sub(d.partitioned_ns());
    parts.push(("the rest of the loop's wall time".to_string(), per(rest)));
    parts.sort_by(|a, b| b.1.total_cmp(&a.1));
    parts.push(("the loop parked on its mailbox".to_string(), per(park)));
    parts
}

fn attribution_rows(d: &Tally, n: usize, rt: Option<f64>) -> Vec<String> {
    let mut rows = vec![
        "| where the bridge's loop spent a round trip | ms | share of the round trip |".to_string(),
        "|---|---:|---:|".to_string(),
    ];
    for (what, ms) in attribution(d, n) {
        rows.push(format!(
            "| {what} | {ms:.3} | {} |",
            rt.and_then(|rt| share(ms, rt))
                .map_or("–".to_string(), |s| format!("{:.1}%", s * 100.0))
        ));
    }
    rows
}

fn tally_json(d: &Tally, n: usize, rt: Option<f64>) -> Value {
    let per = |v: f64| (n > 0).then(|| stats::round3(v / n as f64));
    let mut rows = aterm_json::Map::new();
    for row in TallyRow::ALL {
        let h = d.row(row);
        rows.insert(
            row.name().to_string(),
            json!({
                "unit": if row.unit() == Unit::Ns { "ns" } else { "count" },
                "partitions": row.partitions(),
                "count": h.count,
                "total": h.total,
                "count_per_round_trip": per(h.count as f64),
                "ms_per_round_trip": (row.unit() == Unit::Ns).then(|| per(ns_ms(h.total))).flatten(),
                "max": h.max,
                "bands": h.buckets.to_vec(),
            }),
        );
    }
    let attribution: Vec<Value> = attribution(d, n)
        .into_iter()
        .map(|(what, ms)| {
            json!({
                "what": what,
                "ms_per_round_trip": stats::round3(ms),
                "share_of_round_trip": rt.and_then(|rt| share(ms, rt)),
            })
        })
        .collect();
    json!({
        "format": wait_tally::FORMAT,
        "bands": "log2: band 0 under 1024 ns, band i from 1 [2^(i+9), 2^(i+10)) ns, the last from 2^26 ns; a count's band 0 is 0, band i [2^(i-1), 2^i)",
        "dir_counted": d.dir,
        "bridge_wall_ms": stats::round3(ns_ms(d.at_ns)),
        "bridge_wall_ms_per_round_trip": per(ns_ms(d.at_ns)),
        "rows": Value::Object(rows),
        "attribution": attribution,
    })
}

/// A window's park beside the bench's own spans, each a mean per round trip in ms: aterm's answer
/// to the `inbox` listing a wake is followed by (a real agent lists too) and to `post`, each with
/// its socket, the bench's own turn between them (which a real agent replaces), and what is left,
/// wakes and idle. The loop may be working through part of a span, so [`ParkSplit::rest`] can read
/// below zero; the spans are measured beside the park, not inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ParkSplit {
    park: f64,
    /// B's wake to its listing's reply.
    b_list: f64,
    /// B's listing's reply to its post's request: the bench's own turn.
    b_turn: f64,
    /// A's wake to its listing's reply (outside the round trip).
    a_list: f64,
    /// A's listing's reply to its next post's request: the bench's own turn (outside the round
    /// trip; [`PACE`] included when paced).
    a_turn: f64,
    /// A's post, request to reply.
    a_post: f64,
    /// B's post, request to reply.
    b_post: f64,
}

impl ParkSplit {
    fn of(rounds: &[Round], park_ns: u64) -> Option<Self> {
        let n = rounds.len();
        if n == 0 {
            return None;
        }
        let per = |sum: f64| sum / n as f64;
        let span = |f: fn(&Round) -> (u64, u64)| {
            per(rounds
                .iter()
                .map(|r| {
                    let (a, b) = f(r);
                    ms(a, b)
                })
                .sum::<f64>())
        };
        Some(ParkSplit {
            park: per(ns_ms(park_ns)),
            b_list: span(|r| (r.t_b_seen, r.t_b_listed)),
            b_turn: span(|r| (r.t_b_listed, r.t_b_call)),
            a_list: span(|r| (r.t_a_seen, r.t_a_listed)),
            a_turn: per(rounds
                .windows(2)
                .map(|w| ms(w[0].t_a_listed, w[1].t_call))
                .sum::<f64>()),
            a_post: span(|r| (r.t_call, r.t_ret)),
            b_post: span(|r| (r.t_b_call, r.t_b_ret)),
        })
    }

    fn rest(&self) -> f64 {
        self.park
            - self.b_list
            - self.b_turn
            - self.a_list
            - self.a_turn
            - self.a_post
            - self.b_post
    }

    fn rows(&self) -> Vec<String> {
        vec![
            "| the park, split (each a mean per round trip) | ms |".to_string(),
            "|---|---:|".to_string(),
            format!(
                "| **the loop parked on its mailbox (the bridge's tally)** | {:.3} |",
                self.park
            ),
            format!(
                "| aterm's answer to B's `inbox` after its wake, with its socket | {:.3} |",
                self.b_list
            ),
            format!(
                "| the bench's own turn: B's listing to its post | {:.3} |",
                self.b_turn
            ),
            format!(
                "| aterm's answer to A's `inbox` after its wake, with its socket (outside the round \
                 trip) | {:.3} |",
                self.a_list
            ),
            format!(
                "| the bench's own turn: A's listing to its next post (outside the round trip) | \
                 {:.3} |",
                self.a_turn
            ),
            format!(
                "| aterm's answer to A's `post`, with its socket | {:.3} |",
                self.a_post
            ),
            format!(
                "| aterm's answer to B's `post`, with its socket | {:.3} |",
                self.b_post
            ),
            format!(
                "| the rest: wakes and idle (below zero: the loop worked through part of a span \
                 above) | {:.3} |",
                self.rest()
            ),
        ]
    }

    fn json(&self) -> Value {
        json!({
            "what": "means per round trip in ms, beside the park rather than inside it: aterm's answer to the inbox listing after a wake and to post, each with its socket (b_list, a_list, a_post, b_post), the bench's own turn between them (b_turn, a_turn: which a real agent replaces), and the rest, wakes and idle; the loop may be working through part of a span, so the rest can read below zero",
            "park": stats::round3(self.park),
            "b_list": stats::round3(self.b_list),
            "b_turn": stats::round3(self.b_turn),
            "a_list": stats::round3(self.a_list),
            "a_turn": stats::round3(self.a_turn),
            "a_post": stats::round3(self.a_post),
            "b_post": stats::round3(self.b_post),
            "rest": stats::round3(self.rest()),
        })
    }
}

/// What a row of aterm's handler tally ([`HandlerRow`]) times.
pub(super) const fn describe_handler(row: HandlerRow) -> &'static str {
    match row {
        HandlerRow::Deliver => "`deliver`",
        HandlerRow::WakeWatchers => {
            "of `deliver` and `outbox sent`: ringing the session's watchers"
        }
        HandlerRow::Outbox => "the outbox peek",
        HandlerRow::Sent => "`outbox sent`",
        HandlerRow::Sessions => "`sessions bridge`",
        HandlerRow::OtherBridge => "every other bridge-only verb",
        HandlerRow::Post => "`post`, to its push-lane event and wakes",
        HandlerRow::Inbox => "`inbox`, the listing",
        HandlerRow::AwaitArm => "`await inbox`: entry to its first park",
        HandlerRow::AwaitWake => "`await inbox`: the row in the ring to the answer",
        HandlerRow::FleetMail => {
            "of `inbox`, `await inbox` and a direct post: a fleet-mailbox transaction"
        }
    }
}

/// The bound of the log2 band that holds the nearest-rank `q` quantile of `h`'s values, in µs:
/// the value is below it and, past the first band, at least half of it. The first band holds
/// everything under its bound, zero included; the last band has no bound, only a floor.
pub(super) fn band_quantile(h: &Hist, q: f64) -> String {
    let rank = ((q * h.count as f64).ceil() as u64).max(1);
    let mut seen = 0u64;
    for (i, &n) in h.buckets.iter().enumerate() {
        seen += u64::from(n);
        if h.count > 0 && seen >= rank {
            return if i + 1 < h.buckets.len() {
                format!("< {:.1}", Hist::floor(i + 1, Unit::Ns) as f64 / 1e3)
            } else {
                format!("≥ {:.1}", Hist::floor(i, Unit::Ns) as f64 / 1e3)
            };
        }
    }
    "–".to_string()
}

/// One request, timed from outside and by aterm's handler: what lies between is the socket, the
/// reader thread's wake, and the request's framing and dispatch.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RequestSplit {
    what: &'static str,
    /// The outside clock's calls and their total, in ns.
    calls: u64,
    outside_ns: u64,
    /// The handler's calls and their total, in ns.
    handled: u64,
    handler_ns: u64,
}

impl RequestSplit {
    fn outside_us(&self) -> Option<f64> {
        (self.calls > 0).then_some(self.outside_ns as f64 / self.calls as f64 / 1e3)
    }

    fn handler_us(&self) -> Option<f64> {
        (self.handled > 0).then_some(self.handler_ns as f64 / self.handled as f64 / 1e3)
    }

    fn share(&self) -> Option<f64> {
        if self.handled == 0 {
            return None;
        }
        share(self.handler_ns as f64, self.outside_ns as f64)
    }
}

/// Each verb the bridge's lane times, and the bench's `post`, beside aterm's handler for it.
fn request_splits(waits: &Tally, handlers: &HandlerTally, rounds: &[Round]) -> Vec<RequestSplit> {
    let lane = |what, row: TallyRow, handler: HandlerRow| RequestSplit {
        what,
        calls: waits.row(row).count,
        outside_ns: waits.row(row).total,
        handled: handlers.row(handler).count,
        handler_ns: handlers.row(handler).total,
    };
    let mut splits = vec![
        lane("`deliver`", TallyRow::LaneDeliver, HandlerRow::Deliver),
        lane("the outbox peek", TallyRow::LaneOutbox, HandlerRow::Outbox),
        lane("`outbox sent`", TallyRow::LaneSent, HandlerRow::Sent),
        lane(
            "`sessions bridge`",
            TallyRow::LaneSessions,
            HandlerRow::Sessions,
        ),
        lane(
            "every other verb",
            TallyRow::LaneOther,
            HandlerRow::OtherBridge,
        ),
    ];
    // The presence reads are no bridge-only verbs, so aterm's handler tally times none of them.
    splits.extend(
        [
            ("`@<sid> status`", TallyRow::LaneStatus),
            ("`@<sid> meta`", TallyRow::LaneMeta),
            ("`@<sid> text`", TallyRow::LaneText),
            ("`sessions status`", TallyRow::LaneSessionsStatus),
        ]
        .map(|(what, row)| RequestSplit {
            what,
            calls: waits.row(row).count,
            outside_ns: waits.row(row).total,
            handled: 0,
            handler_ns: 0,
        }),
    );
    let lane_total = splits.iter().fold(
        RequestSplit {
            what: "**the verb lane, all**",
            calls: 0,
            outside_ns: 0,
            handled: 0,
            handler_ns: 0,
        },
        |sum, s| RequestSplit {
            calls: sum.calls + s.calls,
            outside_ns: sum.outside_ns + s.outside_ns,
            handled: sum.handled + s.handled,
            handler_ns: sum.handler_ns + s.handler_ns,
            ..sum
        },
    );
    splits.push(lane_total);
    let post_us: u64 = rounds
        .iter()
        .map(|r| (r.t_ret - r.t_call) + (r.t_b_ret - r.t_b_call))
        .sum();
    splits.push(RequestSplit {
        what: "the bench's `post`",
        calls: 2 * rounds.len() as u64,
        outside_ns: post_us * 1_000,
        handled: handlers.row(HandlerRow::Post).count,
        handler_ns: handlers.row(HandlerRow::Post).total,
    });
    splits
}

fn handler_rows(d: &HandlerTally, n: usize) -> Vec<String> {
    let per = |v: f64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", v / n as f64)
        }
    };
    let mut rows = vec![
        "| aterm's handler (handler tally v=2) | count | per round trip | µs a call | p50 / p95 / \
         p99 (µs, the log2 band's bound) | ms per round trip | ≥ 1.05 ms | largest (ms) |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|---:|---:|".to_string(),
    ];
    for row in HandlerRow::ALL {
        let h = d.row(row);
        rows.push(format!(
            "| {} | {} | {} | {} | {} / {} / {} | {} | {} | {} |",
            describe_handler(row),
            h.count,
            per(h.count as f64),
            if h.count == 0 {
                "–".to_string()
            } else {
                format!("{:.1}", h.total as f64 / h.count as f64 / 1e3)
            },
            band_quantile(h, 0.50),
            band_quantile(h, 0.95),
            band_quantile(h, 0.99),
            per(ns_ms(h.total)),
            h.at_least(TAIL_1MS),
            largest(h),
        ));
    }
    rows
}

fn split_rows(splits: &[RequestSplit], n: usize) -> Vec<String> {
    let us = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.1}"));
    let per = |ns: u64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", ns_ms(ns) / n as f64)
        }
    };
    let mut rows = vec![
        "| request | calls | from outside (µs a call) | aterm's handler (µs a call) | the rest: \
         socket, wake, framing (µs a call) | handler's share | from outside (ms per round trip) \
         | handler (ms per round trip) |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|---:|---:|".to_string(),
    ];
    for s in splits {
        let rest = s
            .outside_us()
            .zip(s.handler_us())
            .map(|(outside, handler)| outside - handler);
        rows.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            s.what,
            s.calls,
            us(s.outside_us()),
            us(s.handler_us()),
            us(rest),
            s.share()
                .map_or("–".to_string(), |v| format!("{:.1}%", v * 100.0)),
            per(s.outside_ns),
            per(s.handler_ns),
        ));
    }
    rows
}

fn handlers_json(d: &HandlerTally, splits: &[RequestSplit], n: usize) -> Value {
    let per = |v: f64| (n > 0).then(|| stats::round3(v / n as f64));
    let mut rows = aterm_json::Map::new();
    for row in HandlerRow::ALL {
        let h = d.row(row);
        rows.insert(
            row.name().to_string(),
            json!({
                "count": h.count,
                "total_ns": h.total,
                "count_per_round_trip": per(h.count as f64),
                "ms_per_round_trip": per(ns_ms(h.total)),
                "max_ns": h.max,
                "bands": h.buckets.to_vec(),
            }),
        );
    }
    let requests: Vec<Value> = splits
        .iter()
        .map(|s| {
            json!({
                "what": s.what,
                "calls": s.calls,
                "outside_us_per_call": s.outside_us().map(stats::round3),
                "handled": s.handled,
                "handler_us_per_call": s.handler_us().map(stats::round3),
                "handler_share": s.share(),
                "outside_ms_per_round_trip": per(ns_ms(s.outside_ns)),
                "handler_ms_per_round_trip": per(ns_ms(s.handler_ns)),
            })
        })
        .collect();
    json!({
        "format": "handler-tally v=2",
        "aterm_wall_ms": stats::round3(ns_ms(d.at_ns)),
        "rows": Value::Object(rows),
        "requests": requests,
    })
}

/// A window's two splits, for the JSON and the entry: the park, and the requests.
fn splits_report(
    rounds: &[Round],
    waits: &Tally,
    handlers: &HandlerTally,
) -> (Value, Value, Vec<String>) {
    let n = rounds.len();
    let splits = request_splits(waits, handlers, rounds);
    let park = ParkSplit::of(rounds, waits.row(TallyRow::Park).total);
    let mut md = vec![String::new()];
    md.extend(park.iter().flat_map(ParkSplit::rows));
    md.push(String::new());
    md.extend(handler_rows(handlers, n));
    md.push(String::new());
    md.extend(split_rows(&splits, n));
    md.push(String::new());
    (
        park.as_ref().map_or(Value::Null, ParkSplit::json),
        handlers_json(handlers, &splits, n),
        md,
    )
}

fn flushes_rows(f: FlushCounts, n: usize) -> Vec<String> {
    let per = |v: u64, by: usize| {
        if by == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", v as f64 / by as f64)
        }
    };
    let row = |what: &str, v: u64| format!("| {what} | {v} | {} | {} |", per(v, 2 * n), per(v, n));
    vec![
        "| broker flushes (`Broker::flush_counts`) | count | per message | per round trip |"
            .to_string(),
        "|---|---:|---:|---:|".to_string(),
        row("barrier (`F_BARRIERFSYNC`)", f.barrier),
        row("full (`F_FULLFSYNC`)", f.full),
        row("full, skipped as already covered", f.skipped),
        row("failed", f.failed),
    ]
}

fn flushes_rt_json(f: FlushCounts, n: usize) -> Value {
    let per = |v: u64| (n > 0).then(|| stats::round3(v as f64 / n as f64));
    let mut out = flushes_json(f, 2 * n);
    if let Some(fields) = out.as_object_mut() {
        fields.insert("barrier_per_round_trip".to_string(), json!(per(f.barrier)));
        fields.insert("full_per_round_trip".to_string(), json!(per(f.full)));
        fields.insert("skipped_per_round_trip".to_string(), json!(per(f.skipped)));
    }
    out
}

fn paced_report(raw: &Raw) -> (Value, Vec<String>) {
    let Some(end) = raw.paced_end else {
        return (Value::Null, Vec::new());
    };
    let n = raw.paced.len();
    let leg = |f: fn(&Round) -> (u64, u64)| -> Vec<f64> {
        raw.paced
            .iter()
            .map(|r| {
                let (a, b) = f(r);
                ms(a, b)
            })
            .collect()
    };
    let round_trip = leg(|r| (r.t_call, r.t_a_seen));
    let ping_leg = leg(|r| (r.t_ret, r.t_b_seen));
    let pong_leg = leg(|r| (r.t_b_ret, r.t_a_seen));
    let turnaround = leg(|r| (r.t_b_seen, r.t_b_call));
    let d = end
        .since(&raw.end)
        .unwrap_or_else(|e| panic!("ping-pong, paced: the bridge's wait tallies do not pair: {e}"));
    let handlers = raw
        .paced_handlers_end
        .expect("taken with the last paced tally")
        .since(&raw.handlers_end)
        .unwrap_or_else(|e| panic!("ping-pong, paced: {e}"));
    let (park_json, handlers_json, splits_md) = splits_report(&raw.paced, &d, &handlers);
    let rt_mean = stats::summarize(&round_trip).map(|s| s.mean);
    let pace_ms = u64::try_from(PACE.as_millis()).unwrap_or(u64::MAX);
    let json = json!({
        "rounds": n,
        "pace_ms": pace_ms,
        "round_trip_ms": stats::to_json(&round_trip),
        "ping_post_to_visible_ms": stats::to_json(&ping_leg),
        "pong_post_to_visible_ms": stats::to_json(&pong_leg),
        "turnaround_ms": stats::to_json(&turnaround),
        "wait_tally": tally_json(&d, n, rt_mean),
        "park_split": park_json,
        "handlers": handlers_json,
    });
    let mut md = vec![
        String::new(),
        "### Paced: the same request and reply on an idle node\n".to_string(),
        format!(
            "{n} more round trips after the back-to-back ones, each {pace_ms} ms after the last \
             one's reply, past the bridge's 5 ms idle backstop: each round finds the loop parked \
             on its mailbox with nothing owed.\n"
        ),
        "| measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("**round trip, paced**", &round_trip),
        stats::row("ping: A's post `OK` -> visible to B", &ping_leg),
        stats::row("pong: B's post `OK` -> visible to A", &pong_leg),
        stats::row(
            "B's turnaround (its wake, its `inbox` listing, its post's request)",
            &turnaround,
        ),
        String::new(),
        format!(
            "The wait tally's rows over the paced window (the bridge's own clock: {:.1} ms, the \
             {pace_ms} ms gaps included; the park holds them):\n",
            ns_ms(d.at_ns)
        ),
    ];
    md.extend(tally_rows(&d, n));
    md.extend(attribution_rows(&d, n, rt_mean));
    md.extend(splits_md);
    (json, md)
}

/// The ping-pong world: boot with the tally on, on aterm's options for the arm's tier ([`boot_world`]), measure, read the log.
pub(super) fn measure(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the ping-pong world");
    let mut w = boot_world(
        "fbench-pp",
        spec,
        &[(wait_tally::SEAM, "1"), (HANDLER_SEAM, "1")],
    );
    w.wait_ready();
    let served = served_line(&w, spec);
    let (a, b) = w.two_sessions();
    let mut live = vec![a.clone(), b.clone()];
    live.extend(bystanders(&w, spec.bystanders));
    wait_live(
        &w,
        &live,
        0,
        "both ping-pong sessions' presence rows, and every bystander's, to say live",
    );
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    eprintln!(
        "fabric bench: ping-pong, {} round trips ({} warm-up)",
        spec.pingpong, spec.warmup
    );
    let (raw, heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let raw = rounds(&w, broker, spec, clock, &a, &b);
        await_durable(broker);
        raw
    });
    let cal_after = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let recs = stop_and_read(&mut w);
    let group = aterm_link::subject::inbox_group(FLEET, &w.node);

    let seq_of = |text: &str, to: &str| -> u64 {
        let lane = format!("/f/{FLEET}/in/{}/{to}/{}/note", w.node, w.node);
        let mut found = recs.iter().filter(|r| r.subject == lane && r.text == text);
        let first = found
            .next()
            .unwrap_or_else(|| panic!("no record of `{text}` on {lane}"));
        if let Some(again) = found.next() {
            panic!(
                "two records on {lane} carry `{text}`, at offsets {} and {}: the bus holds one \
                 message twice",
                first.seq, again.seq
            );
        }
        first.seq
    };
    let n = raw.rounds.len();
    let mut round_trip = Vec::with_capacity(n);
    let mut ping_leg = Vec::with_capacity(n);
    let mut pong_leg = Vec::with_capacity(n);
    let mut turnaround = Vec::with_capacity(n);
    let mut ping_to_acked = Vec::with_capacity(n);
    let mut ping_to_durable = Vec::with_capacity(n);
    let mut pong_to_acked = Vec::with_capacity(n);
    let mut pong_acked_to_visible = Vec::with_capacity(n);
    let mut pong_to_durable = Vec::with_capacity(n);
    let mut pongs = Answers::default();
    let mut pings = Answers::default();
    let behind_of = |delivered: u64, answer: u64, answer_ok: u64| {
        behind(&recs, &group, delivered, answer, answer_ok, |seq| {
            heads.acked.landed(seq)
        })
    };
    let mut previous_pong = None;
    for r in &raw.rounds {
        let ping_seq = seq_of(&r.ping, &b);
        let pong_seq = seq_of(&r.pong, &a);
        assert_eq!(
            (ping_seq, pong_seq),
            (r.ping_off, r.pong_off),
            "`{}`/`{}`: each row's off= is its record's offset",
            r.ping,
            r.pong
        );
        round_trip.push(ms(r.t_call, r.t_a_seen));
        ping_leg.push(ms(r.t_ret, r.t_b_seen));
        pong_leg.push(ms(r.t_b_ret, r.t_a_seen));
        turnaround.push(ms(r.t_b_seen, r.t_b_call));
        ping_to_acked.extend(ms_opt(Some(r.t_ret), heads.acked.landed(ping_seq)));
        ping_to_durable.extend(ms_opt(Some(r.t_ret), heads.durable.landed(ping_seq)));
        let pong_acked = heads.acked.landed(pong_seq);
        pong_to_acked.extend(ms_opt(Some(r.t_b_ret), pong_acked));
        pong_acked_to_visible.extend(ms_opt(pong_acked, Some(r.t_a_seen)));
        pong_to_durable.extend(ms_opt(Some(r.t_b_ret), heads.durable.landed(pong_seq)));
        if let Some(pong) = behind_of(ping_seq, pong_seq, r.t_b_ret) {
            pongs.add(pong);
        }
        if let Some(ping) = previous_pong.and_then(|p| behind_of(p, ping_seq, r.t_ret)) {
            pings.add(ping);
        }
        previous_pong = Some(pong_seq);
    }
    let first = raw.rounds.first().expect("at least one measured round");
    let last = raw.rounds.last().expect("at least one measured round");
    let lo = first.ping_off;
    let cover = census::covering_commit(&recs, &group, last.pong_off, last.pong_off);
    let hi = cover.map_or(last.pong_off, |c| c.seq);
    let t_end = cover.and_then(|c| heads.acked.landed(c.seq));
    let window_ms = t_end.map(|t| ms(first.t_call, t));
    let pp_census = census::census(census::window(&recs, lo, hi));
    let waits = raw
        .end
        .since(&raw.start)
        .unwrap_or_else(|e| panic!("ping-pong: the bridge's wait tallies do not pair: {e}"));
    let handlers = raw
        .handlers_end
        .since(&raw.handlers_start)
        .unwrap_or_else(|e| panic!("ping-pong: {e}"));
    let (park_json, handlers_json, splits_md) = splits_report(&raw.rounds, &waits, &handlers);
    let blocked_ms = ns_ms(waits.blocked_ns());
    let blocked_share = window_ms.and_then(|w| share(blocked_ms, w));
    let p50 = |v: &[f64]| stats::summarize(v).map(|s| s.p50);
    let round_trip_x = per_unit(p50(&round_trip), unit);
    let per_rt = |v: f64| (n > 0).then(|| stats::round3(v / n as f64));
    let rt_mean = stats::summarize(&round_trip).map(|s| s.mean);
    let (paced_json, paced_md) = paced_report(&raw);
    let slow = slow_rounds(&raw);

    let json = json!({
        "node": w.node.clone(),
        "rounds": n,
        "warmup": spec.warmup,
        "served": served.trim_end(),
        "head_changes_sampled": heads.acked.len(),
        "durable_head_changes_sampled": heads.durable.len(),
        "yardstick": yardstick_json(&ruler, &cal_before, &cal_after, unit),
        "round_trip_ms": stats::to_json(&round_trip),
        "round_trip_p50_in_publishes": round_trip_x,
        "ping_post_to_visible_ms": stats::to_json(&ping_leg),
        "pong_post_to_visible_ms": stats::to_json(&pong_leg),
        "turnaround_ms": stats::to_json(&turnaround),
        "ping_post_to_acked_ms": stats::to_json(&ping_to_acked),
        "ping_post_to_durable_head_ms": stats::to_json(&ping_to_durable),
        "pong_post_to_acked_ms": stats::to_json(&pong_to_acked),
        "pong_acked_to_visible_ms": stats::to_json(&pong_acked_to_visible),
        "pong_post_to_durable_head_ms": stats::to_json(&pong_to_durable),
        "flushes": flushes_rt_json(raw.flushes, n),
        "retention_ticks": ticks_json(raw.ticks),
        "behind_commit_ms": {
            "what": "a message's wait for the inbox commit of the delivery it answers: from its post OK to that commit acked, 0 when the commit landed first; only messages the log holds after that commit (one recorded before it was published ahead of it, and did not wait on it)",
            "pong_answering_ping": stats::to_json(&pongs.waits),
            "ping_answering_previous_pong": stats::to_json(&pings.waits),
        },
        "commit_landed_before_the_answer": {
            "pongs": pongs.after,
            "of_pongs": n,
            "pings": pings.after,
            "of_pings": n.saturating_sub(1),
        },
        "answer_recorded_before_the_commit": {
            "what": "messages the log holds before the inbox commit of the delivery they answer: folded when that commit covers the message's own delivery too (one commit for two deliveries, one commit fewer), unfolded when the message's delivery has a later commit of its own",
            "folded": { "pongs": pongs.folded, "pings": pings.folded },
            "unfolded": { "pongs": pongs.ahead, "pings": pings.ahead },
        },
        "window_ms": window_ms.map(stats::round3),
        "bridge_waits": {
            "what": "the bridge loop's waits on broker replies over the window, counted inside the bridge (aterm_link::wait_tally): requests that read their own reply, pipelined acks, and of both the inbox group's commits (a request each before aterm-0, a pipelined ack since)",
            "pid": waits.pid,
            "requests": waits.row(TallyRow::Request).count,
            "request_ms": stats::round3(ns_ms(waits.row(TallyRow::Request).total)),
            "commits": waits.row(TallyRow::Commit).count,
            "commit_ms": stats::round3(ns_ms(waits.row(TallyRow::Commit).total)),
            "acks": waits.row(TallyRow::Ack).count,
            "ack_ms": stats::round3(ns_ms(waits.row(TallyRow::Ack).total)),
            "blocked_ms": stats::round3(blocked_ms),
            "blocked_share_of_window": blocked_share,
            "per_round_trip": {
                "blocked_ms": per_rt(blocked_ms),
                "requests": per_rt(waits.row(TallyRow::Request).count as f64),
                "commits": per_rt(waits.row(TallyRow::Commit).count as f64),
                "acks": per_rt(waits.row(TallyRow::Ack).count as f64),
            },
        },
        "wait_tally": tally_json(&waits, n, rt_mean),
        "park_split": park_json,
        "handlers": handlers_json,
        "bystanders": spec.bystanders,
        "paced": paced_json,
        "slow_calls": tallies::slow_calls_json(&slow),
        "census": census::to_json(&pp_census, 2 * n, (lo, hi)),
    });

    let mut md = vec![
        format!(
            "## Ping-pong: a request and its reply on one node (node `{}`)\n",
            w.node
        ),
        served.clone(),
        format!(
            "{n} round trips after {} warm-up, back to back: A posts `ping-<i>` to B; the moment \
             B's `await inbox` answers, B lists it and posts `pong-<i>` back; the moment A's \
             answers, A lists it and posts the next. Unlike the latency phase, nothing waits for \
             the bridge's inbox commit between messages, so each reply is posted while the \
             commit of the delivery it answers is still owed, and waits on that commit (unless the \
             bridge publishes it first) and on the bridge's one loop. A world of its own, with \
             the bridge's wait tally on. Acked and durable are the broker's two heads, as in the \
             messaging section: on a strict broker one, on an ordered one durable within the \
             horizon, on a relaxed one never.\n",
            spec.warmup
        ),
        "| measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row(
            "**round trip: A's post -> B's reply visible to A**",
            &round_trip,
        ),
        stats::row("ping: A's post `OK` -> visible to B", &ping_leg),
        stats::row("pong: B's post `OK` -> visible to A", &pong_leg),
        stats::row(
            "B's turnaround (its wake, its `inbox` listing, its post's request)",
            &turnaround,
        ),
        stats::row("ping: post `OK` -> acked on the broker", &ping_to_acked),
        stats::row("ping: post `OK` -> durable on the broker", &ping_to_durable),
        stats::row("pong: post `OK` -> acked on the broker", &pong_to_acked),
        stats::row("pong: acked -> visible", &pong_acked_to_visible),
        stats::row("pong: post `OK` -> durable on the broker", &pong_to_durable),
        stats::row(
            "pong's wait for the commit of the ping it answers (B's post `OK` -> that commit \
             acked, 0 when it landed first; only pongs recorded after it)",
            &pongs.waits,
        ),
        stats::row(
            "next ping's wait for the commit of the pong before it (A's post `OK` -> that commit \
             acked; only pings recorded after it)",
            &pings.waits,
        ),
        String::new(),
        order_line(&pongs, &pings, n),
        format!(
            "The bridge's loop, blocked on broker replies, as its wait tally counted it \
             (`aterm_link::wait_tally`) over the window from the first measured post to the \
             commit covering the last reply ({} ms):\n",
            window_ms.map_or("–".to_string(), |w| format!("{w:.1}"))
        ),
    ];
    md.extend(waits_rows(&waits, n));
    md.extend([
        String::new(),
        format!(
            "**Blocked on the broker: {} of the window** ({:.1} ms of {} ms; {} ms per round \
             trip against a round-trip mean of {} ms). The rest is the loop's own work, aterm's \
             and the scheduler's.\n",
            blocked_share.map_or("–".to_string(), |s| format!("{:.1}%", s * 100.0)),
            blocked_ms,
            window_ms.map_or("–".to_string(), |w| format!("{w:.1}")),
            fmt_x(per_rt(blocked_ms)),
            fmt_x(stats::summarize(&round_trip).map(|s| s.mean)),
        ),
        format!(
            "Where the loop's time went, the wait tally's rows over the same window \
             (`aterm_link::wait_tally`; the bridge's own clock: {} ms, {} ms per round trip, \
             {} ms a round trip of it outside every round trip — the bench's own work between \
             rounds, A's listing after each pong, and the window's edges — whose time the rows \
             below hold too, so their shares of the round trip sum past 100%; {} idle \
             bystander session(s) on the node):\n",
            stats::round3(ns_ms(waits.at_ns)),
            fmt_x(per_rt(ns_ms(waits.at_ns))),
            per_rt(ns_ms(waits.at_ns) - round_trip.iter().sum::<f64>())
                .map_or("–".to_string(), |v| format!("{v:.3}")),
            spec.bystanders,
        ),
    ]);
    md.extend(tally_rows(&waits, n));
    md.extend(attribution_rows(&waits, n, rt_mean));
    md.extend(splits_md);
    md.push("The broker's flushes over the same window, read at its two tallies:\n".to_string());
    md.extend(flushes_rows(raw.flushes, n));
    md.push(String::new());
    md.push(ticks_line(raw.ticks));
    md.extend([
        String::new(),
        format!(
            "Records the broker wrote per delivered message, two messages a round trip (offsets \
             {lo}..={hi}, through the commit covering the last reply):\n"
        ),
    ]);
    md.extend(census::rows(&pp_census, 2 * n));
    md.push(String::new());
    md.extend(yardstick_rows(&ruler, &cal_before, &cal_after));
    md.extend([
        String::new(),
        format!(
            "Normalized to the strict yardstick's p50 ({} ms): the round trip's p50 = **{} \
             publishes**.\n",
            fmt_x(unit),
            fmt_x(round_trip_x)
        ),
    ]);
    md.extend(paced_md);
    md.push(String::new());
    md.extend(tallies::slow_calls_tables(SLOW_GROUP, &slow));
    (json, md)
}

#[cfg(test)]
mod tests {
    /// PING-PONG'S SLOW-CALL ROUNDS AND THEIR MARKS (E09c): the back-to-back round runs from
    /// the reading past the warm-up to the one past the last reply and is judged by the first
    /// reading's mark; the paced round runs from there to the reading past its last reply and is
    /// judged by both earlier marks, oldest first; each round's rings are its closing reading's,
    /// the bridge's from its tally and the bench's from the flush times read after it, and its
    /// calls the tallies' difference. With no paced sub-phase there is one round.
    #[test]
    fn the_slow_call_rounds_run_between_the_readings_and_are_judged_by_the_marks_before() {
        use super::*;
        let ms = 1_000_000;
        let tally = |slow_count: u64, calls: u64| {
            let mut t = Tally {
                pid: 5,
                ..Tally::default()
            };
            t.times.slow_count = slow_count;
            for i in 0..calls {
                let (start_ns, ns) = (i * 10 * ms, 2 * ms);
                t.rows[TallyRow::Ack.index()].add(ns, Unit::Ns);
                t.calls.kept[i as usize] = Some(wait_tally::SlowCall {
                    row: TallyRow::Ack,
                    start_ns,
                    ns,
                });
                t.calls.count += 1;
            }
            t
        };
        let bench = |slow_count: u64, at: u64| {
            (
                Flushed {
                    slow_count,
                    ..Flushed::default()
                },
                at,
            )
        };
        let (start, end, paced) = (tally(2, 0), tally(4, 1), tally(7, 3));
        let readings = [bench(3, 10 * ms), bench(5, 20 * ms), bench(9, 30 * ms)];
        let rounds = slow_rounds_of(
            (&start, readings[0]),
            (&end, readings[1]),
            Some((&paced, readings[2])),
        );
        let mark = |at: u64, bench: u64, bridge: u64| Mark {
            at,
            bench,
            bridge_pid: 5,
            bridge,
        };
        let seen: Vec<_> = rounds
            .iter()
            .map(|r| {
                (
                    r.round.as_str(),
                    r.window,
                    r.marks.clone(),
                    r.calls.count,
                    r.bridge.slow_count,
                    r.bench.slow_count,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "back-to-back",
                    (10 * ms, 20 * ms),
                    vec![mark(10 * ms, 3, 2)],
                    1,
                    4,
                    5
                ),
                (
                    "paced",
                    (20 * ms, 30 * ms),
                    vec![mark(10 * ms, 3, 2), mark(20 * ms, 5, 4)],
                    2,
                    7,
                    9
                ),
            ]
        );
        assert!(
            rounds
                .iter()
                .all(|r| r.bridge_pid == 5 && r.group == SLOW_GROUP),
            "the bridge's pid and the report's group"
        );
        let back_to_back = slow_rounds_of((&start, readings[0]), (&end, readings[1]), None);
        assert_eq!(
            back_to_back
                .iter()
                .map(|r| r.round.as_str())
                .collect::<Vec<_>>(),
            ["back-to-back"],
            "no paced sub-phase, one round"
        );
    }

    #[test]
    fn a_tally_is_taken_only_once_it_is_this_bridges_and_past_the_message() {
        use super::*;

        fn tally(pid: u32, committed: u64) -> String {
            let mut t = Tally {
                pid,
                committed,
                ..Tally::default()
            };
            t.rows[TallyRow::Request.index()] = Hist {
                count: 4,
                total: 9_000_000,
                ..Hist::default()
            };
            t.render()
        }

        let line = tally(77, 120);
        let t = covering(Some(&line), 77, 120).expect("through 120");
        assert_eq!(
            (t.pid, t.committed, t.row(TallyRow::Request).count),
            (77, 120, 4)
        );
        assert!(covering(Some(&line), 77, 100).is_ok(), "past it counts");
        for (text, off, pid, why) in [
            (None, 0, 77, "no tally"),
            (Some(line.as_str()), 121, 77, "not yet through 121"),
            (Some(line.as_str()), 120, 78, "pid 77's"),
            (Some(&line[..line.len() / 2]), 0, 77, "unterminated"),
        ] {
            let err = covering(text, pid, off).expect_err(why);
            assert!(err.contains(why), "{why}: {err}");
        }
    }

    #[test]
    fn a_replys_wait_is_counted_only_behind_a_commit_the_log_holds_first() {
        use super::*;
        use crate::census::Rec;
        use astream_broker::store::COMMIT_SUBJECT;

        let group = "/f/f1/cur/n-1/node/inbox";
        let note = "/f/f1/in/n-1/s-a/n-1/note";
        let rec = |seq: u64, subject: &str, upto: Option<u64>| Rec {
            seq,
            producer: (1, seq),
            subject: subject.to_string(),
            text: String::new(),
            head: String::new(),
            commit: upto.map(|u| (group.to_string(), u)),
            bytes: 0,
        };
        let landed = |seq: u64| Some(seq * 1_000);

        let own = [
            rec(10, note, None),
            rec(13, COMMIT_SUBJECT, Some(10)),
            rec(14, note, None),
        ];
        assert_eq!(
            behind(&own, group, 10, 14, 10_000, landed),
            Some(Behind {
                order: Order::After,
                wait: Some(3.0)
            })
        );
        assert_eq!(
            behind(&own, group, 10, 14, 13_500, landed).and_then(|b| b.wait),
            Some(0.0)
        );
        assert_eq!(
            behind(&own, group, 10, 14, 10_000, |_| None),
            Some(Behind {
                order: Order::After,
                wait: None
            })
        );

        let folded = [
            rec(10, note, None),
            rec(11, note, None),
            rec(13, COMMIT_SUBJECT, Some(11)),
        ];
        assert_eq!(
            behind(&folded, group, 10, 11, 10_000, landed),
            Some(Behind {
                order: Order::Folded,
                wait: None
            })
        );
        assert_eq!(
            behind(&folded, group, 10, 14, 10_000, landed),
            Some(Behind {
                order: Order::After,
                wait: Some(3.0)
            })
        );
        let ahead = [
            rec(10, note, None),
            rec(11, note, None),
            rec(13, COMMIT_SUBJECT, Some(10)),
            rec(15, COMMIT_SUBJECT, Some(11)),
        ];
        assert_eq!(
            behind(&ahead, group, 10, 11, 10_000, landed),
            Some(Behind {
                order: Order::Ahead,
                wait: None
            })
        );
        assert_eq!(behind(&own[..1], group, 10, 14, 10_000, landed), None);
    }

    #[test]
    fn the_report_names_a_fold_only_where_the_commit_covered_the_answer() {
        use super::*;

        let tally = |orders: &[Order]| {
            let mut a = Answers::default();
            for &order in orders {
                a.add(Behind {
                    order,
                    wait: (order == Order::After).then_some(1.0),
                });
            }
            a
        };
        let pongs = tally(&[Order::After, Order::Folded, Order::After]);
        let pings = tally(&[Order::After, Order::Ahead]);
        assert_eq!((pongs.after, pongs.folded, pongs.ahead), (2, 1, 0));
        assert_eq!(pongs.waits, vec![1.0, 1.0], "only the answers after count");
        let line = order_line(&pongs, &pings, 3);
        assert!(
            line.starts_with(
                "The commit of the delivery a message answers landed before that message's own \
                 record for 2 of 3 pongs and 1 of 2 pings. "
            ),
            "{line}"
        );
        assert!(
            line.contains(
                "1 pongs and 0 pings were folds, the commit covering their own delivery too"
            ),
            "{line}"
        );
        assert!(
            line.contains("0 pongs and 1 pings were not, their own delivery having a later commit"),
            "{line}"
        );
        let all_after = tally(&[Order::After, Order::After]);
        let line = order_line(&all_after, &tally(&[Order::After]), 2);
        assert!(!line.contains("fold"), "{line}");
        assert!(
            line.ends_with("for 2 of 2 pongs and 1 of 1 pings.\n"),
            "{line}"
        );
    }

    #[test]
    fn the_waits_table_reads_per_round_trip_and_the_share_needs_a_window() {
        use super::*;

        fn with(rows: &[(super::TallyRow, u64, u64)]) -> super::Tally {
            let mut t = super::Tally {
                pid: 1,
                committed: 9,
                dir: true,
                ..super::Tally::default()
            };
            for &(row, count, total) in rows {
                t.rows[row.index()] = super::Hist {
                    count,
                    total,
                    ..super::Hist::default()
                };
            }
            t
        }

        let d = with(&[
            (TallyRow::Request, 400, 1_200_000_000),
            (TallyRow::Commit, 400, 1_100_000_000),
            (TallyRow::Ack, 400, 800_000_000),
        ]);
        let rows = waits_rows(&d, 200);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[2],
            "| requests that read their own reply | 400 | 2.000 | 1200.00 | 6.000 |"
        );
        assert_eq!(
            rows[5],
            "| **all: the loop held by the broker** | – | – | 2000.00 | 10.000 |"
        );
        let mut folded = d;
        folded.rows[TallyRow::Request.index()].count = 399;
        folded.rows[TallyRow::Commit.index()].count = 399;
        let rows = waits_rows(&folded, 200);
        assert!(
            rows[4].starts_with("| of both: the inbox group's commits | 399 | 1.995 |"),
            "{rows:?}"
        );
        assert!(
            rows[3].starts_with(
                "| pipelined acks (publishes', and since aterm-0 the group commits') | 400 | 2.000 |"
            ),
            "{rows:?}"
        );
        assert_eq!(share(2000.0, 4000.0), Some(0.5));
        assert_eq!(share(2000.0, 0.0), None);
        assert!(waits_rows(&d, 0)[2].ends_with("| 400 | – | 1200.00 | – |"));
    }

    #[test]
    fn the_v2_table_reads_each_step_per_round_trip_with_its_tail() {
        use super::*;

        fn with(rows: &[(super::TallyRow, u64, u64)]) -> super::Tally {
            let mut t = super::Tally {
                pid: 1,
                committed: 9,
                dir: true,
                ..super::Tally::default()
            };
            for &(row, count, total) in rows {
                t.rows[row.index()] = super::Hist {
                    count,
                    total,
                    ..super::Hist::default()
                };
            }
            t
        }

        let mut d = with(&[(TallyRow::LaneDeliver, 400, 80_000_000)]);
        let deliver = &mut d.rows[TallyRow::LaneDeliver.index()];
        deliver.buckets[Hist::band(150_000, Unit::Ns)] = 397;
        deliver.buckets[Hist::band(2_000_000, Unit::Ns)] = 2;
        deliver.buckets[Hist::band(9_000_000, Unit::Ns)] = 1;
        deliver.max = 9_000_000;
        d.rows[TallyRow::DirReplace.index()] = Hist {
            count: 400,
            total: 40_000_000,
            max: 0,
            buckets: {
                let mut b = [0; wait_tally::BUCKETS];
                b[Hist::band(100_000, Unit::Ns)] = 400;
                b
            },
        };
        d.rows[TallyRow::DrainPosts.index()] = Hist {
            count: 220,
            total: 200,
            max: 1,
            buckets: {
                let mut b = [0; wait_tally::BUCKETS];
                b[0] = 20;
                b[1] = 200;
                b
            },
        };
        let rows = tally_rows(&d, 200);
        let line = |what: &str| -> String {
            rows.iter()
                .find(|r| r.starts_with(&format!("| {what} |")))
                .unwrap_or_else(|| panic!("no `{what}` row in {rows:?}"))
                .clone()
        };
        assert_eq!(
            line(describe(TallyRow::LaneDeliver)),
            "| verb lane: deliveries (`deliver`) | yes | 400 | 2.000 | 80.00 | 0.400 | 3 | 1 | 9.000 |"
        );
        assert_eq!(
            line(describe(TallyRow::DirReplace)),
            "| state dir: replaces (temp + rename) | yes | 400 | 2.000 | 40.00 | 0.200 | 0 | 0 | < 0.131 |",
            "no new largest in the window: bounded by its top band"
        );
        assert!(
            line(describe(TallyRow::Commit))
                .starts_with("| broker: of those, the inbox group's commits |  | 0 |"),
            "an overlapping row is not marked as partitioning"
        );
        assert!(
            rows.iter().all(|r| !r.contains("posts each outbox peek")),
            "the count row is a sentence, not a time row"
        );
        assert!(
            rows.iter().any(|r| r
                == "Outbox peeks: 220 (1.100 per round trip), listing 200 posts; 20 listed none.\n"),
            "{rows:?}"
        );
        let undir = super::Tally { dir: false, ..d };
        assert!(
            tally_rows(&undir, 200)
                .last()
                .is_some_and(|r| r.contains("were not counted"))
        );
    }

    #[test]
    fn the_attribution_partitions_the_loops_clock_largest_first() {
        use super::*;

        fn with(rows: &[(super::TallyRow, u64, u64)]) -> super::Tally {
            let mut t = super::Tally {
                pid: 1,
                committed: 9,
                dir: true,
                ..super::Tally::default()
            };
            for &(row, count, total) in rows {
                t.rows[row.index()] = super::Hist {
                    count,
                    total,
                    ..super::Hist::default()
                };
            }
            t
        }

        let mut d = with(&[
            (TallyRow::Ack, 200, 100_000_000),
            (TallyRow::Commit, 200, 90_000_000),
            (TallyRow::LaneDeliver, 400, 160_000_000),
            (TallyRow::DirReplace, 400, 30_000_000),
            (TallyRow::SetPostPin, 200, 20_000_000),
            (TallyRow::WakeRecord, 300, 30_000_000),
            (TallyRow::Park, 600, 60_000_000),
        ]);
        d.at_ns = 400_000_000;
        let a = attribution(&d, 200);
        let names: Vec<&str> = a.iter().map(|(w, _)| w.as_str()).collect();
        assert_eq!(names[0], describe(TallyRow::LaneDeliver));
        assert_eq!(names[1], describe(TallyRow::Ack));
        assert_eq!(names[3], describe(TallyRow::DirReplace));
        assert_eq!(*names.last().unwrap(), "the loop parked on its mailbox");
        for overlapping in [TallyRow::Commit, TallyRow::SetPostPin, TallyRow::WakeRecord] {
            assert!(!names.contains(&describe(overlapping)), "{overlapping:?}");
        }
        let ms = |what: &str| a.iter().find(|(w, _)| w == what).unwrap().1;
        assert!((ms(describe(TallyRow::LaneDeliver)) - 0.8).abs() < 1e-9);
        assert!((ms("the rest of the loop's wall time") - 0.25).abs() < 1e-9);
        let sum: f64 = a.iter().map(|(_, ms)| ms).sum();
        assert!(
            (sum - 2.0).abs() < 1e-9,
            "the parts sum to the clock: {sum}"
        );
        let rows = attribution_rows(&d, 200, Some(2.0));
        assert!(
            rows.contains(&"| verb lane: deliveries (`deliver`) | 0.800 | 40.0% |".to_string()),
            "{rows:?}"
        );
        let over = super::Tally {
            at_ns: 100_000_000,
            ..d
        };
        assert_eq!(
            attribution(&over, 200)
                .iter()
                .find(|(w, _)| w.starts_with("the rest"))
                .map(|(_, ms)| *ms),
            Some(0.0)
        );
        assert!(attribution(&d, 0).is_empty());
    }

    #[allow(
        dead_code,
        reason = "the harness-free bench also mounts this module under cfg(test); only the ordinary fabric_bench test target uses this fixture"
    )]
    fn round(base: u64) -> super::Round {
        super::Round {
            ping: String::new(),
            pong: String::new(),
            t_call: base,
            t_ret: base + 300,
            t_b_seen: base + 1_000,
            t_b_listed: base + 1_060,
            t_b_call: base + 1_100,
            t_b_ret: base + 1_500,
            t_a_seen: base + 2_000,
            t_a_listed: base + 2_100,
            ping_off: 0,
            pong_off: 0,
        }
    }

    #[test]
    fn the_park_split_reads_each_span_per_round_trip_and_keeps_the_rest_signed() {
        use super::*;

        let rounds = [round(0), round(2_400), round(4_800), round(7_200)];
        let split = ParkSplit::of(&rounds, 8_000_000).expect("four rounds");
        assert!((split.park - 2.0).abs() < 1e-9);
        assert!(
            (split.b_list - 0.06).abs() < 1e-9,
            "the listing is aterm's, not the bench's turn: {}",
            split.b_list
        );
        assert!((split.b_turn - 0.04).abs() < 1e-9, "{}", split.b_turn);
        assert!((split.a_list - 0.1).abs() < 1e-9, "{}", split.a_list);
        assert!((split.a_post - 0.3).abs() < 1e-9);
        assert!((split.b_post - 0.4).abs() < 1e-9);
        assert!(
            (split.a_turn - 0.225).abs() < 1e-9,
            "three gaps of 0.3 ms over four round trips: {}",
            split.a_turn
        );
        assert!((split.rest() - 0.875).abs() < 1e-9);
        let rows = split.rows();
        assert!(rows[2].contains("2.000"), "{rows:?}");
        assert!(rows[9].ends_with("| 0.875 |"), "{rows:?}");
        let busy = ParkSplit::of(&rounds, 800_000).expect("four rounds");
        assert!(busy.rest() < 0.0, "the loop worked through the spans");
        assert_eq!(ParkSplit::of(&[], 800_000), None);
    }

    #[test]
    fn each_request_splits_into_its_handler_and_the_rest_and_the_lane_sums() {
        use super::*;

        let mut waits = Tally::default();
        waits.rows[TallyRow::LaneDeliver.index()] = Hist {
            count: 400,
            total: 40_000_000,
            ..Hist::default()
        };
        waits.rows[TallyRow::LaneSent.index()] = Hist {
            count: 400,
            total: 20_000_000,
            ..Hist::default()
        };
        let mut handlers = HandlerTally::default();
        handlers.rows[HandlerRow::Deliver.index()] = Hist {
            count: 400,
            total: 10_000_000,
            ..Hist::default()
        };
        handlers.rows[HandlerRow::Post.index()] = Hist {
            count: 4,
            total: 400_000,
            ..Hist::default()
        };
        let rounds = [round(0), round(2_400)];
        let splits = request_splits(&waits, &handlers, &rounds);
        let of = |what: &str| {
            splits
                .iter()
                .find(|s| s.what == what)
                .unwrap_or_else(|| panic!("no `{what}` in {splits:?}"))
        };
        let deliver = of("`deliver`");
        assert_eq!(deliver.outside_us(), Some(100.0));
        assert_eq!(deliver.handler_us(), Some(25.0));
        assert_eq!(deliver.share(), Some(0.25));
        let lane = of("**the verb lane, all**");
        assert_eq!((lane.calls, lane.outside_ns), (800, 60_000_000));
        assert_eq!((lane.handled, lane.handler_ns), (400, 10_000_000));
        let post = of("the bench's `post`");
        assert_eq!(post.calls, 4, "two posts a round trip");
        assert_eq!(post.outside_ns, 2 * (300 + 400) * 1_000);
        assert_eq!(post.handler_us(), Some(100.0));
        let rows = split_rows(&splits, 2);
        assert!(
            rows.iter()
                .any(|r| r == "| `deliver` | 400 | 100.0 | 25.0 | 75.0 | 25.0% | 20.000 | 5.000 |"),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.starts_with("| every other verb | 0 | – | – | – | – |")),
            "{rows:?}"
        );
    }

    #[test]
    fn a_handler_rows_quantiles_name_the_log2_band_that_holds_them() {
        use super::*;

        let mut h = Hist::default();
        for _ in 0..98 {
            h.add(3_000, Unit::Ns);
        }
        h.add(100_000, Unit::Ns);
        h.add(100_000, Unit::Ns);
        assert_eq!(band_quantile(&h, 0.50), "< 4.1");
        assert_eq!(band_quantile(&h, 0.95), "< 4.1");
        assert_eq!(band_quantile(&h, 0.99), "< 131.1", "rank 99 of 100");
        assert_eq!(band_quantile(&Hist::default(), 0.50), "–");
        let mut top = Hist::default();
        top.add(u64::MAX, Unit::Ns);
        assert_eq!(
            band_quantile(&top, 0.50),
            "≥ 67108.9",
            "the last band has no bound"
        );
    }

    #[test]
    fn the_flushes_table_reads_per_message_and_per_round_trip() {
        use super::*;

        let f = FlushCounts {
            barrier: 600,
            full: 4,
            skipped: 1,
            failed: 0,
        };
        let rows = flushes_rows(f, 200);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[2],
            "| barrier (`F_BARRIERFSYNC`) | 600 | 1.500 | 3.000 |"
        );
        assert_eq!(rows[3], "| full (`F_FULLFSYNC`) | 4 | 0.010 | 0.020 |");
        let json = flushes_rt_json(f, 200);
        assert_eq!(json["barrier_per_message"], json!(1.5));
        assert_eq!(json["barrier_per_round_trip"], json!(3.0));
        assert_eq!(json["skipped_per_round_trip"], json!(0.005));
        assert!(flushes_rows(f, 0)[2].ends_with("| 600 | – | – |"));
        assert_eq!(flushes_rt_json(f, 0)["barrier_per_round_trip"], Value::Null);
    }
}
