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
//! next round, once nothing is queued behind it (`Bridge::round`): on Strict,
//! one more broker flush after the row is visible. The latency phase waits for
//! that commit before its next post, so no sample carries the one before it
//! ([`super::latency`]). A reply is posted exactly in that gap. The bridge has
//! ONE loop: it reads the outbox the reply sits in only once the commit's
//! answer is back, and it reads every publish's ack on that same loop. So a
//! reply waits on the delivery it answers and on the loop, and three numbers
//! say how much:
//!
//! 1. THE ROUND TRIP, A's post to B's reply visible to A: `n`, mean, p50, p95,
//!    p99, and its legs.
//! 2. EACH ANSWER'S WAIT FOR THE DELIVERY'S COMMIT. Every message but the first
//!    answers a delivery — a pong the ping before it, the next ping that pong —
//!    so for each, from its post `OK` (it is in aterm's outbox) to the moment
//!    the commit covering the delivery it answers was durable, or zero when
//!    that commit landed first; by direction. The commit's offset is read off
//!    the log after the run and its moment off the head sampler, as every
//!    durable time in this bench is.
//! 3. THE SHARE OF THE WINDOW THE BRIDGE'S LOOP SPENT BLOCKED ON BROKER REPLIES,
//!    counted INSIDE the bridge where it blocks, by the wait-tally development
//!    seam ([`aterm_link::wait_tally`]): the requests that read their own reply
//!    (the group commits among them) and the pipelined acks, in count and time.
//!    The bench takes no time of its own for it: it reads two tallies the
//!    bridge wrote and divides their difference by the window.
//!
//! MEASURED, NEVER JUDGED, like everything here: the checks are that each round
//! is the round it says (each wake is that round's one message, each text is one
//! record whose offset is its row's), and that the two tallies are one bridge
//! process's, the first past the warm-up and the second past the last reply.
//!
//! ITS OWN WORLD. The bridge reads the seam once, at its start, from the
//! environment aterm hands it, so the arm boots a world with it set
//! (`fbench-pp`) and the latency, burst and attach worlds run exactly as they
//! did, with no seam. Inside its world the tally costs the loop one positioned
//! write at an idle moment, and nothing at all while anything is queued.
//!
//! THE WINDOW. After the warm-up rounds the bench waits for a tally whose
//! cursor is past the last warm-up reply — the bridge's idle write after that
//! delivery's commit, so the commit is in it and nothing is owed — and after
//! the measured rounds for one past the last reply. Their difference is what
//! the loop waited on in between, and the window it is a share of runs from the
//! first measured post to the moment the commit covering the last reply was
//! durable.

use std::path::Path;
use std::time::Duration;

use aterm_json::{Value, json};
use aterm_link::ctl::Ctl;
use aterm_link::wait_tally::{self, Tally};

use super::{
    DEADLINE, FLEET, Row, Ruler, World, await_note, calibrate, durability, fmt_x, list_since, ms,
    ms_opt, newest_id, per_unit, post, sampled, stop_and_read, wait_live, yardstick,
    yardstick_rows,
};
use crate::census;
use crate::clock::Clock;
use crate::spec::Spec;
use crate::stats;

/// How often the tally file is read while the bench waits for the bridge's
/// idle write. A file read in the bench process: it asks the broker nothing.
const TALLY_POLL: Duration = Duration::from_millis(1);

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

/// The rounds: `warmup` first, then `pingpong` measured, back to back.
fn rounds(w: &World, spec: &Spec, clock: Clock, a: &str, b: &str) -> Raw {
    let path = w.state.join(wait_tally::FILE);
    let pid = w.bridge_pid();
    let mut ctl = w.ctl();
    let mut a_since = newest_id(&mut ctl, a);
    let mut b_since = newest_id(&mut ctl, b);
    // With no warm-up the window starts at the bridge's last idle write before
    // the first post, wherever its cursor stands.
    let mut start =
        (spec.warmup == 0).then(|| await_tally(&path, pid, 0, "ping-pong, before the first round"));
    let mut measured = Vec::with_capacity(spec.pingpong);
    for i in 0..spec.warmup + spec.pingpong {
        let ping = format!("ping-{i}");
        let pong = format!("pong-{i}");
        let (t_call, t_ret) = post(&mut ctl, clock, a, b, &ping);
        let (ping_row, t_b_seen) = woke_with(&mut ctl, clock, b, &mut b_since, &ping);
        let (t_b_call, t_b_ret) = post(&mut ctl, clock, b, a, &pong);
        let (pong_row, t_a_seen) = woke_with(&mut ctl, clock, a, &mut a_since, &pong);
        let round = Round {
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
        };
        if i + 1 == spec.warmup {
            start = Some(await_tally(
                &path,
                pid,
                round.pong_off,
                "ping-pong, after the warm-up",
            ));
        }
        if i >= spec.warmup {
            measured.push(round);
        }
    }
    let last = measured.last().expect("at least one measured round");
    let end = await_tally(&path, pid, last.pong_off, "ping-pong, after the last round");
    Raw {
        rounds: measured,
        start: start.expect("taken before the first measured round"),
        end,
    }
}

/// How long a reply posted at `reply_ok` waited for the commit that landed at
/// `commit_landed`: the gap, or zero when the commit was durable first. Pure
/// (unit-tested).
fn behind_commit(reply_ok: u64, commit_landed: u64) -> f64 {
    ms(reply_ok, commit_landed).max(0.0)
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
fn waits_rows(d: &Tally, n: usize) -> Vec<String> {
    let per = |v: f64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.2}", v / n as f64)
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
    vec![
        "| the loop waited on | count | per round trip | ms | ms per round trip |".to_string(),
        "|---|---:|---:|---:|---:|".to_string(),
        row(
            "requests that read their own reply",
            Some(d.requests),
            d.request_ns,
        ),
        row(
            "of which: the inbox group's commits",
            Some(d.commits),
            d.commit_ns,
        ),
        row("pipelined publishes' acks", Some(d.acks), d.ack_ns),
        row("**all: the loop held by the broker**", None, d.blocked_ns()),
    ]
}

/// The ping-pong world: boot with the tally on, measure, read the log.
pub(super) fn measure(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the ping-pong world");
    let mut w = World::boot_durable_with(
        "fbench-pp",
        durability(spec.durability),
        &[(wait_tally::SEAM, "1")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    wait_live(
        &w,
        &[a.clone(), b.clone()],
        0,
        "both ping-pong sessions' presence rows to say live",
    );
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w, clock, &mut cal_seq);
    eprintln!(
        "fabric bench: ping-pong, {} round trips ({} warm-up)",
        spec.pingpong, spec.warmup
    );
    let (raw, timeline) = sampled(&w, clock, || rounds(&w, spec, clock, &a, &b));
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
    let mut pong_to_durable = Vec::with_capacity(n);
    let mut pong_durable_to_visible = Vec::with_capacity(n);
    // EVERY MESSAGE BUT THE FIRST ANSWERS A DELIVERY: a pong the ping before
    // it, and a ping the pong of the round before (the first measured ping's
    // was the warm-up's, whose commit the start tally waited out). Each one's
    // wait for that delivery's commit, by direction, and how often the commit
    // landed before the answer's own record.
    let mut pong_behind = Vec::with_capacity(n);
    let mut ping_behind = Vec::with_capacity(n);
    let (mut pong_after_commit, mut ping_after_commit) = (0usize, 0usize);
    // The commit covering `delivered`, landed before `answer`'s record or not,
    // and its durable moment if the sampler saw it.
    let commit_of = |delivered: u64, answer: u64| {
        census::covering_commit(&recs, &group, delivered, delivered)
            .map(|c| (c.seq < answer, timeline.landed(c.seq)))
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
        let pong_landed = timeline.landed(pong_seq);
        pong_to_durable.extend(ms_opt(Some(r.t_b_ret), pong_landed));
        pong_durable_to_visible.extend(ms_opt(pong_landed, Some(r.t_a_seen)));
        if let Some((first, landed)) = commit_of(ping_seq, pong_seq) {
            pong_after_commit += usize::from(first);
            pong_behind.extend(landed.map(|t| behind_commit(r.t_b_ret, t)));
        }
        if let Some((first, landed)) = previous_pong.and_then(|p| commit_of(p, ping_seq)) {
            ping_after_commit += usize::from(first);
            ping_behind.extend(landed.map(|t| behind_commit(r.t_ret, t)));
        }
        previous_pong = Some(pong_seq);
    }
    let first = raw.rounds.first().expect("at least one measured round");
    let last = raw.rounds.last().expect("at least one measured round");
    let lo = first.ping_off;
    let cover = census::covering_commit(&recs, &group, last.pong_off, last.pong_off);
    let hi = cover.map_or(last.pong_off, |c| c.seq);
    let t_end = cover.and_then(|c| timeline.landed(c.seq));
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

    let json = json!({
        "node": w.node.clone(),
        "rounds": n,
        "warmup": spec.warmup,
        "head_changes_sampled": timeline.len(),
        "yardstick": {
            "what": "one bare Strict publish round trip, a client in the bench process to the world's broker, serial",
            "before_ms": stats::to_json(&cal_before),
            "after_ms": stats::to_json(&cal_after),
            "p50_ms": unit.map(stats::round3),
        },
        "round_trip_ms": stats::to_json(&round_trip),
        "round_trip_p50_in_publishes": round_trip_x,
        "ping_post_to_visible_ms": stats::to_json(&ping_leg),
        "pong_post_to_visible_ms": stats::to_json(&pong_leg),
        "turnaround_ms": stats::to_json(&turnaround),
        "pong_post_to_durable_ms": stats::to_json(&pong_to_durable),
        "pong_durable_to_visible_ms": stats::to_json(&pong_durable_to_visible),
        "behind_commit_ms": {
            "what": "a message's wait for the inbox commit of the delivery it answers: from its post OK to that commit durable, 0 when the commit landed first",
            "pong_answering_ping": stats::to_json(&pong_behind),
            "ping_answering_previous_pong": stats::to_json(&ping_behind),
        },
        "commit_landed_before_the_answer": {
            "pongs": pong_after_commit,
            "of_pongs": n,
            "pings": ping_after_commit,
            "of_pings": n.saturating_sub(1),
        },
        "window_ms": window_ms.map(stats::round3),
        "bridge_waits": {
            "what": "the bridge loop's waits on broker replies over the window, counted inside the bridge (aterm_link::wait_tally): requests that read their own reply, of which the inbox group's commits, and pipelined acks",
            "pid": waits.pid,
            "requests": waits.requests,
            "request_ms": stats::round3(ns_ms(waits.request_ns)),
            "commits": waits.commits,
            "commit_ms": stats::round3(ns_ms(waits.commit_ns)),
            "acks": waits.acks,
            "ack_ms": stats::round3(ns_ms(waits.ack_ns)),
            "blocked_ms": stats::round3(blocked_ms),
            "blocked_share_of_window": blocked_share,
            "per_round_trip": {
                "blocked_ms": per_rt(blocked_ms),
                "requests": per_rt(waits.requests as f64),
                "commits": per_rt(waits.commits as f64),
                "acks": per_rt(waits.acks as f64),
            },
        },
        "census": census::to_json(&pp_census, 2 * n, (lo, hi)),
    });

    let mut md = vec![
        format!(
            "## Ping-pong: a request and its reply on one node (node `{}`)\n",
            w.node
        ),
        format!(
            "{n} round trips after {} warm-up, back to back: A posts `ping-<i>` to B; the moment \
             B's `await inbox` answers, B lists it and posts `pong-<i>` back; the moment A's \
             answers, A lists it and posts the next. Unlike the latency phase, nothing waits for \
             the bridge's inbox commit between messages, so each reply is posted while the \
             commit of the delivery it answers is still owed, and waits on that commit and on the \
             bridge's one loop. A world of its own, with the bridge's wait tally on.\n",
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
        stats::row("pong: post `OK` -> durable", &pong_to_durable),
        stats::row("pong: durable -> visible", &pong_durable_to_visible),
        stats::row(
            "pong's wait for the commit of the ping it answers (B's post `OK` -> that commit \
             durable; 0 when it landed first)",
            &pong_behind,
        ),
        stats::row(
            "next ping's wait for the commit of the pong before it (A's post `OK` -> that commit \
             durable)",
            &ping_behind,
        ),
        String::new(),
        format!(
            "The commit of the delivery a message answers landed before that message's own record \
             for {pong_after_commit} of {n} pongs and {ping_after_commit} of {} pings.\n",
            n.saturating_sub(1)
        ),
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
            "Records the broker wrote per delivered message, two messages a round trip (offsets \
             {lo}..={hi}, through the commit covering the last reply):\n"
        ),
    ]);
    md.extend(census::rows(&pp_census, 2 * n));
    md.push(String::new());
    md.extend(yardstick_rows(&cal_before, &cal_after));
    md.extend([
        String::new(),
        format!(
            "Normalized to the yardstick's p50 ({} ms): the round trip's p50 = **{} \
             publishes**.\n",
            fmt_x(unit),
            fmt_x(round_trip_x)
        ),
    ]);
    (json, md)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tally_is_taken_only_once_it_is_this_bridges_and_past_the_message() {
        use super::*;

        fn tally(pid: u32, committed: u64) -> String {
            Tally {
                pid,
                requests: 4,
                request_ns: 9_000_000,
                commits: 2,
                commit_ns: 7_000_000,
                acks: 2,
                ack_ns: 1_000_000,
                committed,
            }
            .render()
        }

        let line = tally(77, 120);
        let t = covering(Some(&line), 77, 120).expect("through 120");
        assert_eq!((t.pid, t.committed, t.requests), (77, 120, 4));
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
    fn a_replys_wait_for_the_commit_is_the_gap_or_nothing() {
        use super::*;

        // The reply queued at 10.000 ms, the commit durable at 13.500 ms.
        assert_eq!(behind_commit(10_000, 13_500), 3.5);
        // NEGATIVE CONTROL: a commit that landed before the reply was posted
        // held it for no time at all, not for a negative one.
        assert_eq!(behind_commit(13_500, 10_000), 0.0);
        assert_eq!(behind_commit(10_000, 10_000), 0.0);
    }

    #[test]
    fn the_waits_table_reads_per_round_trip_and_the_share_needs_a_window() {
        use super::*;

        let d = Tally {
            pid: 1,
            requests: 400,
            request_ns: 1_200_000_000,
            commits: 400,
            commit_ns: 1_100_000_000,
            acks: 400,
            ack_ns: 800_000_000,
            committed: 9,
        };
        let rows = waits_rows(&d, 200);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[2],
            "| requests that read their own reply | 400 | 2.00 | 1200.00 | 6.00 |"
        );
        assert_eq!(
            rows[5],
            "| **all: the loop held by the broker** | – | – | 2000.00 | 10.00 |"
        );
        assert_eq!(share(2000.0, 4000.0), Some(0.5));
        // NEGATIVE CONTROL: no window (the sampler missed the last commit)
        // is no share, never a division by zero.
        assert_eq!(share(2000.0, 0.0), None);
        assert!(waits_rows(&d, 0)[2].ends_with("| 400 | – | 1200.00 | – |"));
    }
}
