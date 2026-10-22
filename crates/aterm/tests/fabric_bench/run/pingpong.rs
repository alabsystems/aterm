// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PING-PONG ARM: a request and its reply, back to back on one node — what
//! one agent asking another and waiting for the answer costs.
//!
//! A posts `ping-<i>` to B; the moment B's `await inbox` answers, B lists it and
//! posts `pong-<i>` back; the moment A's answers, A lists it and posts
//! `ping-<i+1>`. `pingpong=` rounds after the warm-up, one world of their own.
//!
//! WHAT THE LATENCY PHASE LEAVES OUT, ON PURPOSE, AND THIS MEASURES. The bridge
//! delivers a record and commits its inbox cursor past it at the top of its
//! next round, once nothing is queued behind it (`Bridge::settle_owed`): on
//! Strict, one more broker flush after the row is visible. The latency phase
//! waits for that commit before its next post, so no sample carries the one
//! before it ([`super::latency`]). A reply is posted exactly in that gap. The
//! bridge has ONE loop. Until aterm-0 it parked on that commit's answer, and on
//! every owed ack at an idle moment, so it usually read the outbox the reply
//! sat in only once the commit was back (not always: the roster round at the
//! top of a pass reads the outbox too, `Bridge::roster_backstop`, before that
//! pass's round commits, so a reply could be published ahead of the commit it
//! would otherwise wait on). Since, the commit rides the publisher's pipeline
//! and the loop reads an owed ack only where a reply must be judged, where a
//! sender waits on it (a post's, at an idle moment), or once it has arrived:
//! it takes the reply while the commit is in the broker's flush and publishes
//! it behind it, on the same connection. So a reply waits on the broker's
//! order rather than the loop's park, and always on the loop's own work, and
//! three numbers say how much:
//!
//! 1. THE ROUND TRIP, A's post to B's reply visible to A: `n`, mean, p50, p95,
//!    p99, and its legs.
//! 2. EACH ANSWER'S WAIT FOR THE DELIVERY'S COMMIT. Every message but the first
//!    answers a delivery — a pong the ping before it, the next ping that pong —
//!    so for each, from its post `OK` (it is in aterm's outbox) to the moment
//!    the commit covering the delivery it answers was ACKED (on a strict broker
//!    its full flush, on an ordered one its barrier: what the bridge's loop
//!    parked for until aterm-0, and what the answer's record lands behind
//!    since), or zero when that commit landed first; by direction.
//!    The commit's offset is read off the log after the run and its moment off
//!    the acked head's sampler, as every acked time in this bench is. A wait
//!    is counted only for an answer the
//!    log holds AFTER that commit ([`behind`]): one already on the log was
//!    published ahead of it and did not wait for it. Such an answer is a FOLD
//!    only when that commit covers its own delivery too (one commit for two
//!    deliveries, one commit fewer); when its delivery has a later commit of
//!    its own, it is merely ahead, and the round trip still wrote two.
//! 3. THE SHARE OF THE WINDOW THE BRIDGE'S LOOP SPENT BLOCKED ON BROKER REPLIES,
//!    counted INSIDE the bridge where it blocks, by the wait-tally development
//!    seam ([`aterm_link::wait_tally`]): the requests that read their own reply
//!    and the pipelined acks, and the group commits among them (requests until
//!    aterm-0, pipelined acks since), in count and time.
//!    The bench takes no time of its own for it: it reads two tallies the
//!    bridge wrote and divides their difference by the window.
//! 4. WHERE THE REST WENT (the tally's v=2 rows, B10): the loop's verb-lane
//!    round trips by verb, its state-dir writes by kind and by call, each
//!    mailbox item's wait from its push to its take (a wake when the loop was
//!    parked), a post's path from its event to its publish, the loop's park
//!    and the tally's own writes, with the bridge's own clock: so the window
//!    is partitioned, largest first, into the steps that took it and the rest
//!    of the loop's wall time ([`attribution`]), each a share of the round
//!    trip; and the same for a PACED sub-phase whose rounds find the loop
//!    idle ([`PACE`]). The bridge's clock also holds what lies outside every
//!    round trip — the bench's own work between rounds (A's listing after
//!    each pong, [`woke_with`]) and the window's edges — and the rows hold
//!    whatever the loop did then, so the shares sum past 100%; the report
//!    says by how much.
//!
//! EVERY TIER ARM RUNS IT, on aterm's own options for the arm's tier, as the
//! latency, burst and attach worlds do ([`super::boot_world`]): so the arm says
//! what a request and its reply cost on a strict broker, an ordered one at
//! each horizon, and the relaxed ceiling. Each pong's post is timed to its
//! record ACKED and, apart, to its record DURABLE ([`crate::clock`]: on strict
//! the two heads are one, on ordered durable follows at the next full flush,
//! at most the horizon plus a batch and a flush after the first uncovered
//! ack, on relaxed never), and the broker's barrier, full and skipped flushes
//! over the window are counted per delivered message and per round trip
//! ([`astream_broker::Broker::flush_counts`], read at the window's two ends,
//! outside any timed round trip).
//!
//! MEASURED, NEVER JUDGED, like everything here: the checks are that each round
//! is the round it says (each wake is that round's one message, each text is one
//! record whose offset is its row's), and that the two tallies are one bridge
//! process's, the first past the warm-up and the second past the last reply.
//!
//! ITS OWN WORLD. The bridge reads the seam once, at its start, from the
//! environment aterm hands it, so the arm boots a world with it set
//! (`fbench-pp`) and the latency, burst and attach worlds run exactly as they
//! did, with no seam. Inside its world the tally costs the loop a clock read or
//! two per broker reply, one more each time it settles its owed acks, and one
//! positioned write at an idle moment, which a delivery that arrived during
//! that moment's ack settle waits behind ([`aterm_link::wait_tally`] says
//! exactly what): small beside a broker round trip, and not nothing.
//!
//! THE WINDOW. After the warm-up rounds the bench waits for a tally whose
//! cursor is past the last warm-up reply — the bridge's idle write after that
//! delivery's commit, so the commit is in it and nothing is owed — and after
//! the measured rounds for one past the last reply. Their difference is what
//! the loop waited on in between, and the window it is a share of runs from the
//! first measured post to the moment the commit covering the last reply was
//! acked. The broker's flushes are read at the same two tallies.

use std::path::Path;
use std::time::Duration;

use astream_broker::{Broker, FlushCounts};
use aterm_json::{Value, json};
use aterm_link::ctl::Ctl;
use aterm_link::wait_tally::{self, Hist, Row as TallyRow, Tally, Unit};

use super::{
    DEADLINE, FLEET, Row, Ruler, Ticks, World, await_durable, await_note, boot_world, bystanders,
    calibrate, flushes_between, flushes_json, fmt_x, list_since, ms, ms_opt, newest_id, per_unit,
    post, sampled, served_line, stop_and_read, ticks, ticks_between, ticks_json, ticks_line,
    wait_live, yardstick, yardstick_json, yardstick_rows,
};
use crate::census;
use crate::clock::Clock;
use crate::spec::Spec;
use crate::stats;

/// How often the tally file is read while the bench waits for the bridge's
/// idle write. A file read in the bench process: it asks the broker nothing.
const TALLY_POLL: Duration = Duration::from_millis(1);

/// THE PACED SUB-PHASE'S GAP between one round's reply and the next round's
/// post ([`Spec::paced`]): past the bridge's idle backstop (5 ms,
/// `IDLE_ACK_GRACE`), so each round finds the loop parked on its mailbox with
/// nothing owed — a request and its reply on an idle node, where the
/// back-to-back rounds measure a busy one. NOT A SYNCHRONISATION SLEEP: what
/// it precedes waits on nothing it lets pass, it is the measured scenario.
const PACE: Duration = Duration::from_millis(20);

/// One round, as the bench saw it.
#[derive(Debug, Clone)]
struct Round {
    ping: String,
    pong: String,
    /// A's post: before the request, after its `OK`.
    t_call: u64,
    t_ret: u64,
    /// B's `await inbox` answering with the ping.
    t_b_seen: u64,
    /// B's post of the reply: before the request, after its `OK`.
    t_b_call: u64,
    t_b_ret: u64,
    /// A's `await inbox` answering with the reply.
    t_a_seen: u64,
    /// The rows' `off=`: each message's record offset.
    ping_off: u64,
    pong_off: u64,
}

/// What the rounds measured.
struct Raw {
    rounds: Vec<Round>,
    /// The tally past the warm-up, and the one past the last reply.
    start: Tally,
    end: Tally,
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
}

/// One round on `ctl`: A posts `ping-<i>` to B, B's wait answers, B posts
/// `pong-<i>` back, A's wait answers.
fn one_round(
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
    let (ping_row, t_b_seen) = woke_with(ctl, clock, b, b_since, &ping);
    let (t_b_call, t_b_ret) = post(ctl, clock, b, a, &pong);
    let (pong_row, t_a_seen) = woke_with(ctl, clock, a, a_since, &pong);
    Round {
        ping,
        pong,
        t_call,
        t_ret,
        t_b_seen,
        t_b_call,
        t_b_ret,
        t_a_seen,
        ping_off: ping_row.off,
        pong_off: pong_row.off,
    }
}

/// The tally in `text` (the file's contents, if it could be read) when it is
/// bridge `pid`'s and its cursor has committed through `off`; why not
/// otherwise. Pure (unit-tested).
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

/// Park on `sid`'s inbox for the round's one message, `text`, and list it:
/// `(its row, the stamp when the wait answered)`.
///
/// # Panics
///
/// A wait that timed out, or a wake that was not exactly `text`: a stray, or a
/// repeat, would be timed as the reply.
fn woke_with(ctl: &mut Ctl, clock: Clock, sid: &str, since: &mut u64, text: &str) -> (Row, u64) {
    let (woke_at, t) =
        await_note(ctl, clock, sid, *since).unwrap_or_else(|e| panic!("`{text}`: {e}"));
    let rows = list_since(ctl, sid, *since);
    assert!(
        rows.len() == 1 && rows[0].text == text && rows[0].id == woke_at,
        "`{text}`: @{sid} woke at row {woke_at} and listed {rows:?}"
    );
    *since = woke_at;
    (rows[0].clone(), t)
}

/// The rounds: `warmup` first, then `pingpong` measured, back to back, on the
/// world whose broker is `broker`.
fn rounds(w: &World, broker: &Broker, spec: &Spec, clock: Clock, a: &str, b: &str) -> Raw {
    let path = w.state.join(wait_tally::FILE);
    let pid = w.bridge_pid();
    let mut ctl = w.ctl();
    let mut a_since = newest_id(&mut ctl, a);
    let mut b_since = newest_id(&mut ctl, b);
    // With no warm-up the window starts at the bridge's last idle write before
    // the first post, wherever its cursor stands. The flush counts are read
    // right after each window tally: the commit it covers is acked, and the
    // next post has not been made.
    let mut start = (spec.warmup == 0).then(|| {
        (
            await_tally(&path, pid, 0, "ping-pong, before the first round"),
            broker.flush_counts(),
            ticks(broker),
        )
    });
    let mut measured = Vec::with_capacity(spec.pingpong);
    for i in 0..spec.warmup + spec.pingpong {
        let round = one_round(&mut ctl, clock, a, b, i, &mut a_since, &mut b_since);
        if i + 1 == spec.warmup {
            start = Some((
                await_tally(&path, pid, round.pong_off, "ping-pong, after the warm-up"),
                broker.flush_counts(),
                ticks(broker),
            ));
        }
        if i >= spec.warmup {
            measured.push(round);
        }
    }
    let last = measured.last().expect("at least one measured round");
    let end = await_tally(&path, pid, last.pong_off, "ping-pong, after the last round");
    let flushes_end = broker.flush_counts();
    let ticks_end = ticks(broker);
    let (start, flushes_start, ticks_start) = start.expect("taken before the first measured round");
    // THE PACED SUB-PHASE: the same request and reply, each round [`PACE`]
    // after the last one's reply, so each finds the loop idle and parked.
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
    Raw {
        rounds: measured,
        start,
        end,
        flushes: flushes_between(flushes_start, flushes_end),
        ticks: ticks_between(ticks_start, ticks_end),
        paced,
        paced_end,
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
    /// of its own, later. Published ahead, and no commit fewer.
    Ahead,
}

/// What the log says of one answer and the commit of the delivery it answers.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Behind {
    order: Order,
    /// The answer's wait for that commit, in ms: from its post `OK` to the
    /// commit acked, 0 when it was acked before the post. `None` when the
    /// order is not [`Order::After`], and when the head sampler never saw the
    /// commit land.
    wait: Option<f64>,
}

/// [`Behind`] for the answer at offset `answer`, posted `OK` at `answer_ok`,
/// to the delivery at offset `delivered`, off the stopped log `recs` (`group`
/// is the inbox group) and the moments `landed` stamps on its offsets; `None`
/// when no commit in the log covers the delivery. Pure (unit-tested).
///
/// WHY THE ORDER GATES THE WAIT. An answer's record can reach the log before
/// the commit of the delivery it answers (`2026-10-17-fabric-36`: one pong of
/// 200). The gap from its post to that commit is then no wait of the answer's:
/// it was published ahead of the commit, and counting the gap would bill it
/// for a commit it was never queued behind. Only an answer recorded after the
/// commit ([`Order::After`]) waited on it.
///
/// AND WHY AHEAD IS NOT ALWAYS A FOLD. Recorded ahead says only that the
/// bridge published the answer first. Whether that commit covered the
/// answer's delivery too (one commit for two: -36's 399 commits over 200 round
/// trips) is the commit's cursor, read off the same log by the census's one
/// rule for covering ([`census::covering_commit`]): the answer's own covering
/// commit is that very commit ([`Order::Folded`]), or a later one
/// ([`Order::Ahead`]).
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

/// The report's sentence on where the answers sat against the commits of the
/// deliveries they answer, over `n` round trips (`n` pongs, `n - 1` pings).
/// Pure (unit-tested).
///
/// A FOLD IS NAMED ONLY WHERE THE LOG SHOWS ONE ([`Order::Folded`]). The
/// sentence goes into an append-only ledger, and an answer recorded ahead of a
/// commit that covers only the delivery it answers is no fold: the round trip
/// still wrote its two commits.
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

/// `part` as a share of `whole`, rounded: `None` for an empty whole. Pure
/// (unit-tested).
fn share(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| stats::round3(part / whole))
}

/// Nanoseconds as milliseconds.
fn ns_ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// The table of what the loop waited on: count and time, in all and per round
/// trip. Pure (unit-tested).
///
/// PER ROUND TRIP IN THREE DECIMALS, as the census's per-message column is: a
/// count that is not a whole number per round trip must not print as one. Two
/// decimals printed 399 commits over 200 round trips as "2.00"
/// (`2026-10-17-fabric-36`), which reads as the 2 a round trip that never
/// folds a commit would have; it is 1.995.
///
/// THE GROUP COMMITS ARE A ROW OF THEIR OWN, under both kinds of wait: before
/// aterm-0 each was a request the loop parked on for the broker's flush
/// (every entry through `2026-10-20-fabric-15` counts them among the
/// requests); since, each rides the publisher's pipeline and its ack is read
/// like a publish's, waited for only where the bridge must (its
/// `Bridge::settle_owed` lists where) and otherwise once it has arrived — so a
/// tally says which by where the commits' count sits.
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

/// The band at and past which a span is counted as a TAIL in the v=2 table:
/// 2^20 ns (1.05 ms), past any quiet verb-lane round trip or state-dir write
/// this bench has seen, and 2^23 ns (8.39 ms), past a quiet volume's whole
/// horizon full flush (5-7 ms, `docs/measured/flush-wait-2026-10-21.md`), so
/// what is counted there met one under load or waited out more than one.
const TAIL_1MS: usize = 11;
const TAIL_8MS: usize = 14;

/// What a v=2 row counts, for the table. Pure.
fn describe(row: TallyRow) -> &'static str {
    match row {
        TallyRow::Request => "broker: requests that read their own reply",
        TallyRow::Ack => "broker: pipelined acks read (publishes', the group commits')",
        TallyRow::Commit => "broker: of those, the inbox group's commits",
        TallyRow::LaneOutbox => "verb lane: the outbox peek",
        TallyRow::LaneSessions => "verb lane: a drain's roster read (`sessions bridge`)",
        TallyRow::LaneSent => "verb lane: a post's retirement (`outbox sent`)",
        TallyRow::LaneDeliver => "verb lane: deliveries (`deliver`)",
        TallyRow::LaneOther => "verb lane: every other verb",
        TallyRow::DirReplace => "state dir: replaces (temp + rename)",
        TallyRow::DirUnlink => "state dir: unlinks",
        TallyRow::DirAppend => "state dir: appends",
        TallyRow::DirFlush => "state dir: flushes, of a file in place or a directory",
        TallyRow::SetPostPin => "of the dir's: a post's reservation (`set_post_pin`)",
        TallyRow::SetInFlight => "of the dir's: a self-lane post's entry (`set_in_flight`)",
        TallyRow::ClearInFlight => "of the dir's: the entry's removal (`clear_in_flight`)",
        TallyRow::ClearPostSeq => "of the dir's: the reservation forgotten (`clear_post_seq`)",
        TallyRow::AppendSelfAcked => "of the dir's: the landed offset (`append_self_acked`)",
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

/// A row's largest in a window, in ms: exact when the window saw the
/// process's largest, else bounded by its top band ([`Hist::top`]). Pure.
fn largest(h: &Hist) -> String {
    if h.max > 0 {
        return format!("{:.3}", ns_ms(h.max));
    }
    match h.top(Unit::Ns) {
        None => "–".to_string(),
        Some((_, Some(bound))) => format!("< {:.3}", ns_ms(bound)),
        Some((floor, None)) => format!("≥ {:.3}", ns_ms(floor)),
    }
}

/// THE V=2 TABLE: every timed row of the window's tally difference `d` over
/// `n` round trips — count and time, in all and per round trip, the tail
/// past 1.05 and 8.39 ms ([`TAIL_1MS`], [`TAIL_8MS`]) and the largest — the
/// rows that partition the loop's time marked, and the outbox peeks' post
/// counts as a sentence of their own. Pure (unit-tested).
fn tally_rows(d: &Tally, n: usize) -> Vec<String> {
    let per = |v: f64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", v / n as f64)
        }
    };
    let mut rows = vec![
        "| the loop's step (wait tally v=2) | partitions | count | per round trip | ms | ms per \
         round trip | ≥ 1.05 ms | ≥ 8.39 ms | largest (ms) |"
            .to_string(),
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
            " The state dir's rows were not counted: a build without debug assertions."
        }
    ));
    rows
}

/// WHERE THE LOOP'S TIME WENT over the window `d`, per round trip and
/// largest first: each row that partitions it ([`TallyRow::partitions`]) but
/// the park, then THE REST of its working time — the bridge's own clock
/// between the two tallies ([`Tally::at_ns`]) less the park and those rows:
/// wall time outside the measured spans, including scheduling delays and
/// instrumentation overhead — and last the park. No CPU clock is sampled.
/// Each in ms per round trip. Pure (unit-tested).
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

/// [`attribution`] as a table, each share of the round trip's mean `rt`.
/// Pure (unit-tested).
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

/// The window's v=2 rows for the report's JSON: each timed row's count, ms,
/// per round trip, its bands and its largest; the peeks' counts; the bridge's
/// own clock; and the attribution with its shares of `rt`.
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
        "format": "wait-tally v=2",
        "bands": "log2: band 0 under 1024 ns, band i from 1 [2^(i+9), 2^(i+10)) ns, the last from 2^26 ns; a count's band 0 is 0, band i [2^(i-1), 2^i)",
        "dir_counted": d.dir,
        "bridge_wall_ms": stats::round3(ns_ms(d.at_ns)),
        "bridge_wall_ms_per_round_trip": per(ns_ms(d.at_ns)),
        "rows": Value::Object(rows),
        "attribution": attribution,
    })
}

/// The table of the broker's flushes over the window, `n` round trips (two
/// delivered messages each): each kind's count, per delivered message and per
/// round trip. Pure (unit-tested).
///
/// WHAT EACH ROW SAYS OF A TIER. A strict broker spends a full flush on every
/// batch, so its barrier row is zero; an ordered one a barrier per batch, plus
/// a full flush per escalated batch and per horizon, and a full flush is
/// SKIPPED when the durable head already covers it; a relaxed one flushes
/// nothing a ping-pong writes (aterm's table escalates no session's note).
/// Three decimals, for the same reason as [`waits_rows`].
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

/// The window's flushes for the report's JSON: [`flushes_json`] per delivered
/// message (two a round trip), and the same per round trip.
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

/// THE PACED SUB-PHASE'S REPORT ([`Spec::paced`]): its round trips and legs,
/// and the loop's v=2 rows over its window, from the back-to-back rounds'
/// last tally to the tally past its last reply. Each round's park holds the
/// [`PACE`] gap before it, so read the working rows per round trip beside the
/// back-to-back ones; the attribution's park is mostly the gap. Nothing when
/// the run takes no paced round.
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
    let rt_mean = stats::summarize(&round_trip).map(|s| s.mean);
    let pace_ms = u64::try_from(PACE.as_millis()).unwrap_or(u64::MAX);
    let json = json!({
        "rounds": n,
        "pace_ms": pace_ms,
        "round_trip_ms": stats::to_json(&round_trip),
        "ping_post_to_visible_ms": stats::to_json(&ping_leg),
        "pong_post_to_visible_ms": stats::to_json(&pong_leg),
        "turnaround_ms": stats::to_json(&turnaround),
        "wait_tally_v2": tally_json(&d, n, rt_mean),
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
        stats::row("B's turnaround (the bench: wake, list, post)", &turnaround),
        String::new(),
        format!(
            "The wait tally's rows over the paced window (the bridge's own clock: {:.1} ms, the \
             {pace_ms} ms gaps included; the park holds them):\n",
            ns_ms(d.at_ns)
        ),
    ];
    md.extend(tally_rows(&d, n));
    md.extend(attribution_rows(&d, n, rt_mean));
    md.push(String::new());
    (json, md)
}

/// The ping-pong world: boot with the tally on, on aterm's options for the
/// arm's tier ([`boot_world`]), measure, read the log.
pub(super) fn measure(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the ping-pong world");
    let mut w = boot_world("fbench-pp", spec, &[(wait_tally::SEAM, "1")]);
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
    let cal_before = calibrate(&ruler, &w, clock, &mut cal_seq);
    eprintln!(
        "fabric bench: ping-pong, {} round trips ({} warm-up)",
        spec.pingpong, spec.warmup
    );
    // The rounds, then (on an ordered broker) the wait for the horizon's full
    // flush, so the last pongs are seen durable before the sampler stops: after
    // the window's last tally, never inside it.
    let (raw, heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let raw = rounds(&w, broker, spec, clock, &a, &b);
        await_durable(broker);
        raw
    });
    let cal_after = calibrate(&ruler, &w, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let recs = stop_and_read(&mut w);
    let group = aterm_link::subject::inbox_group(FLEET, &w.node);

    // ONE RECORD PER TEXT, on the lane its message took, at its row's offset.
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
    // EVERY MESSAGE BUT THE FIRST ANSWERS A DELIVERY: a pong the ping before
    // it, and a ping the pong of the round before (the first measured ping's
    // was the warm-up's, whose commit the start tally waited out). How often
    // that delivery's commit landed before the answer's own record, by
    // direction, and for each answer it landed before, the answer's wait for
    // it ([`behind`]: one recorded first did not wait on it, and was a fold
    // only when that commit covered its own delivery too).
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
    let blocked_ms = ns_ms(waits.blocked_ns());
    let blocked_share = window_ms.and_then(|w| share(blocked_ms, w));
    let p50 = |v: &[f64]| stats::summarize(v).map(|s| s.p50);
    let round_trip_x = per_unit(p50(&round_trip), unit);
    let per_rt = |v: f64| (n > 0).then(|| stats::round3(v / n as f64));
    let rt_mean = stats::summarize(&round_trip).map(|s| s.mean);
    let (paced_json, paced_md) = paced_report(&raw);

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
        "wait_tally_v2": tally_json(&waits, n, rt_mean),
        "bystanders": spec.bystanders,
        "paced": paced_json,
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
        stats::row("B's turnaround (the bench: wake, list, post)", &turnaround),
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
            "Where the loop's time went, the wait tally's v=2 rows over the same window \
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
    md.extend([
        String::new(),
        "The broker's flushes over the same window, read at its two tallies:\n".to_string(),
    ]);
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
    (json, md)
}

#[cfg(test)]
mod tests {
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
        // NEGATIVE CONTROLS: not written yet, not yet through the message,
        // another process's, and a line a write was racing.
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
        };
        // Every record durable at 1 ms per offset: offset 13 at 13.000 ms.
        let landed = |seq: u64| Some(seq * 1_000);

        // ITS OWN COMMIT: the ping at 10, the commit through it at 13, the
        // pong at 14. The pong queued at 10.000 ms waited 3 ms for it.
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
        // A commit durable before the reply was posted held it for no time at
        // all, not for a negative one; one the sampler missed has no wait.
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

        // THE FOLD, the case this guards: the ping at 10, the pong already at
        // 11, and ONE commit at 13 through both. The commit is durable 3 ms
        // after the pong was posted, but the pong was on the log before it: it
        // never waited on that commit, so no wait is counted, where the gap
        // alone would have counted 3 ms.
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
        // NEGATIVE CONTROL: the same log and the same moments, read for an
        // answer recorded after that commit (offset 14), is a 3 ms wait — the
        // fold's `None` is the record order, not the clock.
        assert_eq!(
            behind(&folded, group, 10, 14, 10_000, landed),
            Some(Behind {
                order: Order::After,
                wait: Some(3.0)
            })
        );
        // PUBLISHED AHEAD, NO FOLD: the pong already at 11, but the commit at
        // 13 runs only through the ping (the bridge published the pong before
        // it had delivered it), and the pong's delivery gets its own at 15.
        // Recorded before the commit, so no wait — and no fold either: the
        // round trip wrote both its commits. Read as a fold, a run of these
        // would print "one commit for two deliveries" beside a full count.
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
        // No commit in the log covers the delivery: nothing to say.
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
        // -36's shape on 3 round trips: one pong folded, the rest after.
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
        // NEGATIVE CONTROL: every answer after its commit says nothing of
        // folds, nor of answers recorded ahead.
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

        /// A v=2 window: each row a count and a total.
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
        // ONE FOLDED COMMIT is visible per round trip: 399 commits over 200
        // round trips (`2026-10-17-fabric-36`) reads 1.995, where two decimals
        // printed the 2.00 of a run with no fold.
        let mut folded = d;
        folded.rows[TallyRow::Request.index()].count = 399;
        folded.rows[TallyRow::Commit.index()].count = 399;
        let rows = waits_rows(&folded, 200);
        assert!(
            rows[4].starts_with("| of both: the inbox group's commits | 399 | 1.995 |"),
            "{rows:?}"
        );
        // CONTROL: a count that did not fold, the acks, reads a whole 2.000.
        assert!(
            rows[3].starts_with(
                "| pipelined acks (publishes', and since aterm-0 the group commits') | 400 | 2.000 |"
            ),
            "{rows:?}"
        );
        assert_eq!(share(2000.0, 4000.0), Some(0.5));
        // NEGATIVE CONTROL: no window (the sampler missed the last commit)
        // is no share, never a division by zero.
        assert_eq!(share(2000.0, 0.0), None);
        assert!(waits_rows(&d, 0)[2].ends_with("| 400 | – | 1200.00 | – |"));
    }

    /// THE V=2 TABLE: a row per timed step, its tail past 1.05 and 8.39 ms
    /// from its bands, its largest exact when the window saw it and bounded by
    /// its top band otherwise; the peeks' posts as a sentence, its empty peeks
    /// counted from band 0.
    #[test]
    fn the_v2_table_reads_each_step_per_round_trip_with_its_tail() {
        use super::*;

        /// A v=2 window: each row a count and a total.
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
        // NEGATIVE CONTROL: a line whose dir rows were not counted says so.
        let undir = super::Tally { dir: false, ..d };
        assert!(
            tally_rows(&undir, 200)
                .last()
                .is_some_and(|r| r.contains("were not counted"))
        );
    }

    /// THE ATTRIBUTION: the partitioning rows but the park, largest first,
    /// then the REST of the loop's working time (its clock less the park and
    /// those rows), then the park — per round trip, each a share of the round
    /// trip's mean. Overlapping rows (a commit, a call row, a wake) are not
    /// in it; a window whose rows outrun its clock leaves no negative rest.
    #[test]
    fn the_attribution_partitions_the_loops_clock_largest_first() {
        use super::*;

        /// A v=2 window: each row a count and a total.
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
        // 400 ms of clock less 60 park, 100 ack, 160 deliver, 30 replace.
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
        // NEGATIVE CONTROL: rows past the clock leave a rest of zero, never a
        // negative share; no round trip is no attribution.
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

    /// THE FLUSHES READ PER MESSAGE AND PER ROUND TRIP, the message column
    /// over twice the round trips: an ordered window of 200 round trips with
    /// 600 barriers (a ping, a pong and a commit's batch each), 4 horizon full
    /// flushes and one skipped is 1.500 barriers a message and 3.000 a round
    /// trip. The JSON carries the same two divisions.
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
        // NEGATIVE CONTROL: no round trip is no division, never a NaN or an
        // infinity in an append-only ledger.
        assert!(flushes_rows(f, 0)[2].ends_with("| 600 | – | – |"));
        assert_eq!(flushes_rt_json(f, 0)["barrier_per_round_trip"], Value::Null);
    }
}
