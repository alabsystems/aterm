// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HANDOFF ARM.

use std::collections::BTreeMap;
use std::time::Duration;

use aterm_json::{Value, json};

use super::{
    DEADLINE, FLEET, PID_POLL, attach_rows, await_attach, await_note, boot_world, list_since, ms,
    ms_opt, newest_id, post, presence_subject, read_pid, sampled, served_line, stop_and_read,
};
use crate::census;
use crate::clock::{Clock, Heads};
use crate::harness::{self, kv, until};
use crate::spec::Spec;
use crate::stats;

/// The QA seam that makes a quit say `bye handoff` (`aterm-gui`
/// `fabric_launch.rs` `QUIT_BYE_SEAM`): every instance of the world gets it,
/// so each holder in turn hands off.
const QUIT_BYE: (&str, &str) = ("ATERM_DEBUG_QUIT_BYE", "handoff");

/// How often a standing-by bridge tries the held lock: `aterm-link`'s
/// `STANDBY_POLL` (`bridge.rs`, private there), spelled here for
/// [`phase_of`] alone.
const STANDBY_POLL: Duration = Duration::from_secs(1);

/// WHERE IN THE SUCCESSOR'S POLL round `n` of `rounds` (1-based) quits: the
/// holder waits `(n - 1) / rounds` of [`STANDBY_POLL`] after the standby was
/// seen and the round set up.
fn phase_of(n: usize, rounds: usize) -> Duration {
    let rounds = u32::try_from(rounds.max(1)).unwrap_or(u32::MAX);
    let step = u32::try_from(n.saturating_sub(1)).unwrap_or(u32::MAX) % rounds;
    STANDBY_POLL * step / rounds
}

/// What one handoff's window measured, inside it.
struct Timed {
    /// The successor bridge's pid, once it took the dir.
    pid: i32,
    /// Before the `invoke Quit` to the holder.
    t_quit: u64,
    /// The old bridge seen gone.
    t_gone: u64,
    /// The gap's post answered `OK`.
    t_post: u64,
    /// Whether the state dir's pid file still named the old bridge (or none)
    /// right after that `OK`: the post was queued in the gap.
    in_gap: bool,
    /// The successor's pid file seen.
    t_pid: u64,
    /// The gap's post visible in its recipient's inbox.
    t_seen: u64,
}

/// One handoff: what the bench knew before it, what it timed, and the heads
/// sampled over its window.
struct Round {
    /// The visible head before the quit: every record of the handoff is at
    /// or past it.
    mark: u64,
    /// The successor's two sessions, the gap post's sender and recipient.
    from: String,
    to: String,
    /// The gap post's text.
    gap: String,
    timed: Timed,
    heads: Heads,
}

/// One handoff, timed: the holder `w` quits saying `bye handoff` while
/// `succ` stands by; `from` posts `gap` to `to` the moment the old bridge is
/// gone.
fn hand_off(
    w: &harness::World,
    succ: &harness::Successor,
    clock: Clock,
    old_pid: i32,
    (from, to, gap): (&str, &str, &str),
    mark: u64,
    phase: Duration,
) -> Timed {
    let node_subject = presence_subject(w, "node");
    let presence_filter = format!("/f/{FLEET}/pub/{}/*/presence", w.node);
    let session_subjects = [presence_subject(w, from), presence_subject(w, to)];
    let mut ctl = succ.ctl();
    let since = newest_id(&mut ctl, to);
    let mut watch = w
        .god()
        .subscribe(mark, &presence_filter)
        .unwrap_or_else(|e| panic!("subscribe to {presence_filter} from {mark}: {e}"));
    watch
        .set_read_timeout(Some(DEADLINE))
        .expect("bound the presence subscription");

    std::thread::sleep(phase);
    let t_quit = clock.us();
    let _ = w.ctl().request("invoke Quit");
    aterm_spec::wait::within(DEADLINE, PID_POLL, || {
        (!harness::alive(old_pid)).then_some(())
    })
    .unwrap_or_else(|| panic!("the old bridge (pid {old_pid}) did not exit within {DEADLINE:?}"));
    let t_gone = clock.us();
    let (_, t_post) = post(&mut ctl, clock, from, to, gap);
    let in_gap = read_pid(w).is_none_or(|p| p == old_pid);
    let pid =
        aterm_spec::wait::within(DEADLINE, PID_POLL, || read_pid(w).filter(|p| *p != old_pid))
            .unwrap_or_else(|| panic!("the successor's bridge took no dir within {DEADLINE:?}"));
    let t_pid = clock.us();
    let (woke_at, t_seen) =
        await_note(&mut ctl, clock, to, since).unwrap_or_else(|e| panic!("`{gap}`: {e}"));
    let listed = list_since(&mut ctl, to, since);
    assert!(
        listed.len() == 1 && listed[0].text == gap && listed[0].id == woke_at,
        "`{gap}`: @{to} woke at row {woke_at}, listed {listed:?}"
    );
    await_attach(&mut watch, &node_subject, &session_subjects, pid);
    Timed {
        pid,
        t_quit,
        t_gone,
        t_post,
        in_gap,
        t_pid,
        t_seen,
    }
}

/// The handoff world: boot, hand the node off `rounds` times, read the log.
pub(super) fn measure(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the handoff world");
    let mut w = boot_world("fbench-ho", spec, &[QUIT_BYE]);
    w.wait_ready();
    let served = served_line(&w, spec);
    let node_subject = presence_subject(&w, "node");
    eprintln!("fabric bench: handoff, {} round(s)", spec.rounds);
    let mut rounds = Vec::with_capacity(spec.rounds);
    for n in 1..=spec.rounds {
        let succ = w.successor_instance(n, &[QUIT_BYE]);
        until("the successor's bridge to stand by on the held dir", || {
            let h = succ.fabric_status();
            (kv(&h, "state") == Some("stalled") && kv(&h, "reason") == Some("state-locked"))
                .then_some(())
        });
        let spawned = succ
            .ctl()
            .request("spawn")
            .unwrap_or_else(|e| panic!("spawn on the successor: {e}"));
        assert!(spawned.ok(), "spawn on the successor: {}", spawned.header());
        let sids: Vec<String> = until("the successor to host two sessions", || {
            let rows = succ.sessions();
            (rows.len() >= 2).then(|| rows.into_iter().map(|(_, sid, _)| sid).collect())
        });
        let (from, to) = (sids[0].clone(), sids[1].clone());
        let gap = format!("gap-{n}");
        let old_pid = w.bridge_pid();
        let mark = w
            .broker
            .as_ref()
            .expect("the world serves its own broker")
            .visible_head();
        let (timed, heads) = sampled(&w, clock, || {
            hand_off(
                &w,
                &succ,
                clock,
                old_pid,
                (&from, &to, &gap),
                mark,
                phase_of(n, spec.rounds),
            )
        });
        until("the old instance to exit after its quit", || {
            w.gui.as_mut()?.try_wait().ok().flatten()
        });
        succ.take(&mut w);
        rounds.push(Round {
            mark,
            from,
            to,
            gap,
            timed,
            heads,
        });
    }
    let recs = stop_and_read(&mut w);
    let fired = census::will_firings(&recs);

    let names = [
        "quit -> the old bridge gone",
        "**the old bridge gone -> the successor's every row acked (the outage)**",
        "the old bridge gone -> the successor's pid (it took the dir)",
        "the successor's pid -> its every row acked (its attach)",
        "the old bridge gone -> its will acked",
        "**the gap's post -> visible**",
        "the gap's post -> acked",
        "the gap's post acked -> visible",
    ];
    let mut series: [Vec<f64>; 8] = Default::default();
    let mut detail = Vec::with_capacity(rounds.len());
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut totals = Vec::with_capacity(rounds.len());
    let mut fired_counts = Vec::with_capacity(rounds.len());
    let n_rounds = rounds.len();
    for (i, r) in rounds.iter().enumerate() {
        let t = &r.timed;
        let acked = |seq: u64| r.heads.acked.landed(seq);
        let rows: Vec<(u64, &str, &str)> = recs
            .iter()
            .filter(|rec| rec.seq >= r.mark)
            .map(|rec| (rec.seq, rec.subject.as_str(), rec.head.as_str()))
            .collect();
        let subjects = [presence_subject(&w, &r.from), presence_subject(&w, &r.to)];
        let found = attach_rows(&rows, &node_subject, &subjects, t.pid).unwrap_or_else(|| {
            panic!(
                "the log holds no complete attach by the successor (pid {}) past offset {}",
                t.pid, r.mark
            )
        });
        let last = found.last();
        let t_all = acked(last);
        let lane = format!("/f/{FLEET}/in/{}/{}/{}/note", w.node, r.to, w.node);
        let mut posted = recs
            .iter()
            .filter(|rec| rec.subject == lane && rec.text == r.gap);
        let gap_seq = posted
            .next()
            .unwrap_or_else(|| panic!("no record of `{}` on {lane}", r.gap))
            .seq;
        assert!(posted.next().is_none(), "`{}` is on {lane} twice", r.gap);
        assert!(
            gap_seq > found.node,
            "`{}` landed at offset {gap_seq}, before the successor's node row at {}",
            r.gap,
            found.node
        );
        let will = recs
            .iter()
            .find(|rec| {
                rec.seq >= r.mark && rec.subject == node_subject && fired.contains(&rec.seq)
            })
            .map(|rec| rec.seq);
        let t_will = will.and_then(acked);
        let t_gap_acked = acked(gap_seq);
        series[0].push(ms(t.t_quit, t.t_gone));
        series[1].extend(ms_opt(Some(t.t_gone), t_all));
        series[2].push(ms(t.t_gone, t.t_pid));
        series[3].extend(ms_opt(Some(t.t_pid), t_all));
        series[4].extend(ms_opt(Some(t.t_gone), t_will));
        series[5].push(ms(t.t_post, t.t_seen));
        series[6].extend(ms_opt(Some(t.t_post), t_gap_acked));
        series[7].extend(ms_opt(t_gap_acked, Some(t.t_seen)));
        let window = || census::window(&recs, r.mark, last).filter(|rec| rec.seq != gap_seq);
        let c = census::census(window().filter(|rec| !fired.contains(&rec.seq)));
        for (class, n) in &c.by_class {
            *kinds.entry(class.clone()).or_default() += n;
        }
        totals.push(c.total as f64);
        let fired_here = window().filter(|rec| fired.contains(&rec.seq)).count();
        fired_counts.push(fired_here as f64);
        detail.push(json!({
            "mark": r.mark,
            "successor_pid": t.pid,
            "node_offset": found.node,
            "last_offset": last,
            "gap_post_offset": gap_seq,
            "will_offset": will,
            "gap_post_queued_in_gap": t.in_gap,
            "records": c.total,
            "fired_wills": fired_here,
            "phase_ms": phase_of(i + 1, n_rounds).as_millis(),
            "quit_to_gone_ms": stats::round3(ms(t.t_quit, t.t_gone)),
            "outage_ms": ms_opt(Some(t.t_gone), t_all).map(stats::round3),
            "gone_to_pid_ms": stats::round3(ms(t.t_gone, t.t_pid)),
            "gap_post_to_visible_ms": stats::round3(ms(t.t_post, t.t_seen)),
        }));
    }
    let n = rounds.len();
    let in_gap = rounds.iter().filter(|r| r.timed.in_gap).count();
    let by_kind: Vec<String> = kinds
        .iter()
        .map(|(class, k)| format!("`{class}` {:.1}", *k as f64 / n.max(1) as f64))
        .collect();
    let mut md = vec![
        format!(
            "## Handoff: the node's bridge handed to a successor's (node `{}`)\n",
            w.node
        ),
        served.clone(),
        format!(
            "A seamless update's bridge handoff, its bridges' half (a headless instance cannot \
             update): a successor instance armed with the same fabric command comes up on the \
             same `--state`, its bridge STANDING BY on the held dir (`link down \
             reason=state-locked`, the lock tried once a second); the holder quits saying `bye \
             handoff`; the moment its bridge is gone (`kill(pid, 0)` refused, read every \
             millisecond) one successor session posts a note to the other, queued while no \
             bridge carries the node; the successor's bridge takes the dir, attaches (its node \
             and both sessions' presence rows, under its own pid and incarnation) and drains the \
             post. Then the successor holds the node and the next round hands it on. Round k of \
             n quits (k - 1)/n of the 1 s poll later than the first, counted from the standby \
             seen and the round set up, so the rounds sample where in the poll the exit lands \
             rather than one point of it. Times are \
             read off the log afterwards, as the attach phase's are. {n} handoff(s); the gap's \
             post was queued before the successor took the dir in {in_gap} of them.\n"
        ),
        "| measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
    ];
    for (name, values) in names.iter().zip(series.iter()) {
        md.push(stats::row(name, values));
    }
    md.push(String::new());
    md.push(format!(
        "Records the broker wrote per handoff (mean over {n}), from the mark to the successor's \
         last row, the gap's post and the old bridge's fired will apart: {:.1} — {}; fired wills \
         {:.1}.\n",
        totals.iter().sum::<f64>() / n.max(1) as f64,
        by_kind.join(" · "),
        fired_counts.iter().sum::<f64>() / n.max(1) as f64,
    ));
    let json_kinds: aterm_json::Map = kinds
        .into_iter()
        .map(|(class, k)| (class, json!(stats::round3(k as f64 / n.max(1) as f64))))
        .collect();
    let json = json!({
        "node": w.node.clone(),
        "served": served.trim_end(),
        "handoffs": n,
        "gap_posts_queued_in_gap": in_gap,
        "quit_to_gone_ms": stats::to_json(&series[0]),
        "outage_ms": stats::to_json(&series[1]),
        "gone_to_pid_ms": stats::to_json(&series[2]),
        "pid_to_all_ms": stats::to_json(&series[3]),
        "gone_to_will_ms": stats::to_json(&series[4]),
        "gap_post_to_visible_ms": stats::to_json(&series[5]),
        "gap_post_to_acked_ms": stats::to_json(&series[6]),
        "gap_post_acked_to_visible_ms": stats::to_json(&series[7]),
        "records_per_handoff": stats::to_json(&totals),
        "records_by_kind_mean": Value::Object(json_kinds),
        "fired_wills_not_counted": stats::to_json(&fired_counts),
        "rounds_detail": Value::Array(detail),
    });
    (json, md)
}

#[cfg(test)]
mod tests {

    /// THE ROUNDS SPREAD OVER ONE POLL: quits at once, and each later
    /// one a further `1/rounds` of the poll in, never a whole poll.
    #[test]
    fn the_rounds_quit_spread_over_one_standby_poll() {
        use std::time::Duration;

        use super::{STANDBY_POLL, phase_of};

        let ms = |n, rounds| phase_of(n, rounds).as_millis();
        assert_eq!([ms(1, 3), ms(2, 3), ms(3, 3)], [0, 333, 666]);
        assert_eq!(ms(1, 1), 0, "one round quits at once");
        assert_eq!([ms(1, 4), ms(2, 4), ms(3, 4), ms(4, 4)], [0, 250, 500, 750]);
        assert_eq!(phase_of(5, 4), Duration::ZERO, "a fifth of four wraps");
        assert!((1..=7).all(|n| phase_of(n, 7) < STANDBY_POLL));
        assert_eq!(phase_of(1, 0), Duration::ZERO, "no rounds is one round");
    }
}
