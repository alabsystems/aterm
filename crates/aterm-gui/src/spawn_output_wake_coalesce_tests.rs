// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    HoldRelease, LoopIdle, OUTPUT_CADENCE_NS, OutputWakeVerdict, OutputWakeWatch, PostCadence,
    STREAM_AFTER_NS, STREAM_AFTER_POSTS, WAKE_LATCH_DIRTY, WAKE_LATCH_EXPIRY_NS, WAKE_LATCH_HELD,
    WAKE_LATCH_SENT, WAKE_LATCH_STAMP, WAKE_LATCH_STREAMING, WakeGate, gated_output_wake,
    judge_output_wake, output_latch_after_pass, release_output_hold, wake_stranded,
};
use std::sync::atomic::{AtomicU64, Ordering};

const MS: u64 = 1_000_000;
const US: u64 = 1_000;

#[test]
fn a_burst_streams_after_a_frame_and_a_quiet_frame_ends_it() {
    let mut c = PostCadence::default();
    assert!(!c.streaming(5 * MS), "a reader's first post");
    let mut t = 5 * MS;
    c.posted(t);
    assert!(!c.streaming(t + OUTPUT_CADENCE_NS));
    t += OUTPUT_CADENCE_NS;
    c.posted(t);
    let from = t;
    while t + 2 * MS < from + STREAM_AFTER_NS {
        t += 2 * MS;
        assert!(!c.streaming(t), "+{} µs into the burst", (t - from) / US);
        c.posted(t);
    }
    t += 2 * MS;
    assert!(c.streaming(t), "a frame into the burst");
    c.posted(t);
    t += 30 * MS;
    c.held(t);
    assert!(
        c.streaming(t + STREAM_AFTER_NS - 1),
        "a stream that stalled under a frame resumes as one"
    );
    assert!(
        !c.streaming(t + STREAM_AFTER_NS),
        "a quiet frame ends the burst"
    );
    c.posted(t + STREAM_AFTER_NS);
    assert!(!c.streaming(t + STREAM_AFTER_NS + 100 * US), "a new burst");
}

#[test]
fn a_streaming_post_takes_a_hold_and_a_lone_one_clears() {
    let lone = AtomicU64::new(0);
    assert_eq!(
        gated_output_wake(&lone, 10 * MS, 0, false, || true, || 10 * MS),
        WakeGate::Posted
    );
    let word = lone.load(Ordering::Relaxed);
    assert_eq!(word, (10 * MS) | WAKE_LATCH_SENT);
    assert_eq!(
        output_latch_after_pass(word, 10 * MS),
        0,
        "a lone pass clears"
    );

    let streaming = AtomicU64::new(0);
    let mut during = 0;
    assert_eq!(
        gated_output_wake(
            &streaming,
            20 * MS,
            0,
            true,
            || {
                during = streaming.load(Ordering::Relaxed);
                true
            },
            || 20 * MS + 1,
        ),
        WakeGate::Posted
    );
    assert_eq!(during, (20 * MS) | WAKE_LATCH_STREAMING, "the unsent arm");
    let word = streaming.load(Ordering::Relaxed);
    assert_eq!(word, (20 * MS + 1) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING);
    let held = output_latch_after_pass(word, 20 * MS + 50 * US);
    assert_eq!(
        held,
        WAKE_LATCH_HELD | (20 * MS + 50 * US + OUTPUT_CADENCE_NS)
    );
    assert_eq!(
        held & WAKE_LATCH_SENT,
        0,
        "a hold is never a sent wake, so the watchdog never judges it"
    );
    assert_eq!(
        output_latch_after_pass(held | WAKE_LATCH_DIRTY, 30 * MS),
        WAKE_LATCH_HELD | (30 * MS + OUTPUT_CADENCE_NS)
    );
}

#[test]
fn a_held_latch_coalesces_into_its_release() {
    let due = 50 * MS;
    let latch = AtomicU64::new(WAKE_LATCH_HELD | due);
    assert_eq!(
        release_output_hold(&latch, due - 1),
        HoldRelease::Pending { due_ns: due }
    );
    let mut sends = 0;
    for at in [47 * MS, 48 * MS, 49 * MS] {
        let gate = gated_output_wake(
            &latch,
            at,
            0,
            true,
            || {
                sends += 1;
                true
            },
            || at,
        );
        assert_eq!(gate, WakeGate::Held);
    }
    assert_eq!(sends, 0, "a hold posts nothing");
    assert_eq!(
        latch.load(Ordering::Relaxed),
        WAKE_LATCH_HELD | WAKE_LATCH_DIRTY | due
    );
    assert_eq!(
        release_output_hold(&latch, due - 1),
        HoldRelease::Pending { due_ns: due },
        "dirty, but not due"
    );
    assert_eq!(release_output_hold(&latch, due), HoldRelease::Due);
    let word = latch.load(Ordering::Relaxed);
    latch.store(output_latch_after_pass(word, due), Ordering::Relaxed);
    let due2 = due + OUTPUT_CADENCE_NS;
    assert_eq!(latch.load(Ordering::Relaxed), WAKE_LATCH_HELD | due2);
    assert_eq!(release_output_hold(&latch, due2), HoldRelease::Released);
    assert_eq!(latch.load(Ordering::Relaxed), 0);
    assert_eq!(release_output_hold(&latch, due2), HoldRelease::NotHeld);
    assert_eq!(
        gated_output_wake(&latch, due2 + MS, 0, false, || true, || due2 + MS),
        WakeGate::Posted
    );
    assert_eq!(release_output_hold(&latch, u64::MAX), HoldRelease::NotHeld);
}

#[test]
fn the_watchdog_never_judges_a_hold() {
    let mut watch = OutputWakeWatch::default();
    for word in [
        WAKE_LATCH_HELD | (5 * MS),
        WAKE_LATCH_HELD | WAKE_LATCH_DIRTY | (5 * MS),
    ] {
        assert_eq!(
            judge_output_wake(word, u64::MAX, 900 * MS),
            OutputWakeVerdict::Clear
        );
        assert!(!wake_stranded(word, 1, 900 * MS));
        let latch = AtomicU64::new(word);
        watch.pass(1_000 * MS, true, [(4, &latch)]);
        let pass = watch.pass(2_000 * MS, true, [(4, &latch)]);
        assert_eq!((pass.healed, pass.late, pass.recheck_ns), (0, 0, None));
        assert_eq!(latch.load(Ordering::Relaxed), word, "untouched");
    }
    let lost = AtomicU64::new((5 * MS) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING);
    let mut watch = OutputWakeWatch::default();
    watch.pass(10 * MS, true, [(4, &lost)]);
    assert_eq!(watch.pass(20 * MS, true, [(4, &lost)]).healed, 1);
    assert_eq!(lost.load(Ordering::Relaxed), 0);
    assert!(
        wake_stranded(
            (5 * MS) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING,
            1,
            900 * MS
        ),
        "the streaming bit is not part of the stamp"
    );
    assert_eq!(WAKE_LATCH_STAMP & (WAKE_LATCH_HELD | WAKE_LATCH_DIRTY), 0);
}

fn drive(reads: &[u64]) -> (Vec<u64>, usize, u64) {
    drive_with_edges(reads, &[])
}

fn drive_with_edges(reads: &[u64], edges: &[u64]) -> (Vec<u64>, usize, u64) {
    fn pass(latch: &AtomicU64, now: u64, passes: &mut Vec<u64>) {
        let word = latch.load(Ordering::Relaxed);
        latch.store(output_latch_after_pass(word, now), Ordering::Relaxed);
        passes.push(now);
    }
    let latch = AtomicU64::new(0);
    let mut cadence = PostCadence::default();
    let (mut passes, mut folds) = (Vec::new(), 0);
    let (mut queued, mut deadline): (Option<u64>, Option<u64>) = (None, None);
    for &read in reads.iter().chain(std::iter::once(&u64::MAX)) {
        loop {
            let wake_turn = queued.filter(|&q| deadline.is_none_or(|d| q <= d));
            let Some(now) = wake_turn.or(deadline).filter(|&at| at <= read) else {
                break;
            };
            if wake_turn.is_some() {
                queued = None;
                pass(&latch, now, &mut passes);
            }
            deadline = match release_output_hold(&latch, now) {
                HoldRelease::Pending { due_ns } => Some(due_ns),
                HoldRelease::Due => {
                    pass(&latch, now, &mut passes);
                    Some(now + OUTPUT_CADENCE_NS)
                }
                HoldRelease::NotHeld | HoldRelease::Released => None,
            };
            folds += usize::from(deadline.is_some());
        }
        if read == u64::MAX {
            break;
        }
        if edges.contains(&read) {
            cadence.foreground_edge(&latch);
        }
        let gate = gated_output_wake(&latch, read, 0, cadence.streaming(read), || true, || read);
        match gate {
            WakeGate::Posted | WakeGate::Reposted => {
                cadence.posted(read);
                queued = Some(read + 20 * US);
            }
            WakeGate::Held => cadence.held(read),
            WakeGate::Coalesced => {}
        }
    }
    (passes, folds, latch.load(Ordering::Relaxed))
}

fn assert_covered(reads: &[u64], passes: &[u64], bound: u64) {
    for &read in reads {
        let pass = passes.iter().find(|&&p| p > read).copied();
        assert!(
            pass.is_some_and(|p| p - read <= bound),
            "the chunk fed at {} µs waits for {pass:?}",
            read / US
        );
    }
}

#[test]
fn a_line_per_read_writer_costs_one_pass_per_cadence() {
    let start = 1_000 * MS;
    let reads: Vec<u64> = (0..2_000).map(|i| start + i * 100 * US).collect();
    let (passes, folds, latch) = drive(&reads);
    let bound = (STREAM_AFTER_NS / (100 * US)
        + (200 * MS - STREAM_AFTER_NS) / OUTPUT_CADENCE_NS
        + 4) as usize;
    assert!(
        passes.len() <= bound,
        "{} passes for {} reads (at most {bound})",
        passes.len(),
        reads.len()
    );
    assert!(folds > 0, "the holds folded their deadlines");
    assert_covered(&reads, &passes, OUTPUT_CADENCE_NS);
    assert_eq!(latch, 0, "quiet: nothing held");
}

#[test]
fn a_stream_whose_writer_stalls_keeps_its_holds() {
    let start = 1_000 * MS;
    let reads: Vec<u64> = (0..4_000u64)
        .map(|i| start + i * 100 * US + (i / 200) * 6 * MS)
        .collect();
    let (passes, folds, latch) = drive(&reads);
    let span = reads.last().copied().unwrap_or(start) - start;
    let stalls = (reads.len() / 200) as u64;
    let bound = (STREAM_AFTER_NS / (100 * US) + span / OUTPUT_CADENCE_NS + 2 * stalls + 4) as usize;
    assert!(
        passes.len() <= bound,
        "{} passes for {} reads (at most {bound})",
        passes.len(),
        reads.len()
    );
    assert!(folds > 0, "the holds folded their deadlines");
    assert_covered(&reads, &passes, OUTPUT_CADENCE_NS);
    assert_eq!(latch, 0, "quiet: nothing held");
}

#[test]
fn a_dense_burst_streams_after_its_posts_and_an_echo_never_does() {
    let mut c = PostCadence::default();
    let from = 5 * MS;
    let mut t = from;
    for post in 0..STREAM_AFTER_POSTS {
        assert!(!c.streaming(t), "post {post}");
        c.posted(t);
        t += 100 * US;
    }
    assert!(c.streaming(t), "the post after the dense count");
    assert!(t - from < STREAM_AFTER_NS, "well short of a frame");
    let quiet = t - 100 * US + STREAM_AFTER_NS - 1;
    assert!(
        c.streaming(quiet),
        "a dense burst outlives a stall under a frame"
    );

    let mut echo = PostCadence::default();
    let mut t = 900 * MS;
    for _ in 0..7 {
        assert!(!echo.streaming(t));
        echo.posted(t);
        t += 40 * US;
    }
    assert!(!echo.streaming(t), "seven reads are no flood");
    assert!(
        !echo.streaming(t - 40 * US + OUTPUT_CADENCE_NS),
        "a quiet cadence ends a burst that is neither old nor dense"
    );
}

#[test]
fn a_stream_that_stalls_before_its_first_frame_still_holds() {
    let start = 1_000 * MS;
    let reads: Vec<u64> = (0..4_000u64)
        .map(|i| start + i * 100 * US + (i / 80) * 5 * MS)
        .collect();
    let (passes, folds, latch) = drive(&reads);
    let span = reads.last().copied().unwrap_or(start) - start;
    let stalls = (reads.len() / 80) as u64;
    let bound =
        (u64::from(STREAM_AFTER_POSTS) + span / OUTPUT_CADENCE_NS + 2 * stalls + 4) as usize;
    assert!(
        passes.len() <= bound,
        "{} passes for {} reads (at most {bound})",
        passes.len(),
        reads.len()
    );
    assert!(folds > 0, "the holds folded their deadlines");
    assert_covered(&reads, &passes, OUTPUT_CADENCE_NS);
    assert_eq!(latch, 0, "quiet: nothing held");
}

#[test]
fn a_prompt_after_a_program_s_output_is_never_held() {
    let start = 1_000 * MS;
    let flood: Vec<u64> = (0..400u64).map(|i| start + i * 100 * US).collect();
    let lines: Vec<u64> = (0..40u64).map(|i| start + i * 300 * US).collect();
    for (output, gap) in [(&flood, MS), (&lines, 500 * US), (&lines, 6 * MS)] {
        let at = output.last().copied().unwrap_or(start) + gap;
        let prompt: Vec<u64> = (0..3).map(|r| at + r * 200 * US).collect();
        let reads: Vec<u64> = output.iter().chain(&prompt).copied().collect();
        let (passes, _, latch) = drive_with_edges(&reads, &[at]);
        assert_covered(&prompt, &passes, 20 * US);
        assert_eq!(latch, 0, "quiet: nothing held");
    }
}

#[test]
fn a_typed_echo_is_never_held() {
    let reads: Vec<u64> = (0..40)
        .flat_map(|key| (0..7).map(move |r| 1_000 * MS + key * 80 * MS + r * 40 * US))
        .collect();
    let (passes, folds, latch) = drive(&reads);
    assert_eq!(passes.len(), reads.len(), "a pass per read, as before");
    assert_eq!(folds, 0, "no hold, so no extra turn");
    assert_covered(&reads, &passes, 20 * US);
    assert_eq!(latch, 0);
}

#[test]
fn the_app_runs_a_dirty_holds_pass_at_its_release() {
    use std::time::{Duration, Instant};
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let epoch = app.lat_epoch;
    let ns = |t: Instant| u64::try_from(t.duration_since(epoch).as_nanos()).expect("ns");
    let latch = app
        .pool
        .get(0)
        .expect("session 0")
        .output_wake_pending
        .clone();
    let t0 = Instant::now();
    latch.store(
        ns(t0) | WAKE_LATCH_SENT | WAKE_LATCH_STREAMING,
        Ordering::Relaxed,
    );
    app.on_output_wake(0, t0);
    let due = ns(t0) + OUTPUT_CADENCE_NS;
    assert_eq!(latch.load(Ordering::Relaxed), WAKE_LATCH_HELD | due);
    assert_eq!(
        app.release_output_holds(t0).map(ns),
        Some(due),
        "a standing hold folds its due instant"
    );
    assert_eq!(
        crate::spawn::gated_output_wake_for_test(&latch, ns(t0) + 1, || {
            panic!("a hold posts nothing")
        }),
        WakeGate::Held
    );
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .pending_deco_birth = None;
    let at_due = epoch + Duration::from_nanos(due);
    assert_eq!(
        app.release_output_holds(at_due).map(ns),
        Some(due + OUTPUT_CADENCE_NS),
        "the release's pass holds again"
    );
    assert_eq!(
        app.windows[&wid].pending_deco_birth,
        Some(at_due),
        "the release ran the session's pass"
    );
    assert_eq!(
        latch.load(Ordering::Relaxed),
        WAKE_LATCH_HELD | (due + OUTPUT_CADENCE_NS)
    );
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .pending_deco_birth = None;
    let at_due2 = at_due + Duration::from_nanos(OUTPUT_CADENCE_NS);
    assert_eq!(app.release_output_holds(at_due2), None);
    assert_eq!(latch.load(Ordering::Relaxed), 0);
    assert_eq!(app.windows[&wid].pending_deco_birth, None);
    latch.store(ns(at_due2) | WAKE_LATCH_SENT, Ordering::Relaxed);
    app.on_output_wake(0, at_due2);
    assert_eq!(latch.load(Ordering::Relaxed), 0);
    assert_eq!(app.release_output_holds(at_due2), None);
}

#[test]
fn failed_send_resets_the_flag() {
    let flag = AtomicU64::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    drop(rx);
    let gate = gated_output_wake(&flag, 1, 0, false, || tx.send(()).is_ok(), || 2);
    assert_eq!(gate, WakeGate::Coalesced, "nothing was posted");
    assert_eq!(
        flag.load(Ordering::Relaxed),
        0,
        "failed send must not leave the latch armed"
    );
    let (tx2, rx2) = std::sync::mpsc::channel::<()>();
    let gate = gated_output_wake(&flag, 3, 0, false, || tx2.send(()).is_ok(), || 4);
    assert_eq!(gate, WakeGate::Posted);
    assert_eq!(rx2.try_iter().count(), 1);
}

#[test]
fn the_arm_is_stamped_after_the_post() {
    let flag = AtomicU64::new(0);
    let mut during = 0;
    gated_output_wake(
        &flag,
        10 * MS,
        0,
        false,
        || {
            during = flag.load(Ordering::Relaxed);
            true
        },
        || 11 * MS,
    );
    assert_eq!(during, 10 * MS, "unsent while the post is in progress");
    assert_eq!(flag.load(Ordering::Relaxed), (11 * MS) | WAKE_LATCH_SENT);
}

#[test]
fn a_handler_clear_during_the_post_stands() {
    let flag = AtomicU64::new(0);
    gated_output_wake(
        &flag,
        5,
        0,
        false,
        || {
            flag.store(0, Ordering::Relaxed);
            true
        },
        || 6,
    );
    assert_eq!(flag.load(Ordering::Relaxed), 0);
}

fn reachable(m: &aterm_spec::derive::Model) -> Vec<aterm_spec::interp::State> {
    let mut frontier = vec![m.init_state()];
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(frontier[0].clone());
    let mut out = Vec::new();
    while let Some(st) = frontier.pop() {
        for a in &m.actions {
            for nxt in m.successors(a.name, &st) {
                if seen.insert(nxt.clone()) {
                    frontier.push(nxt);
                }
            }
        }
        out.push(st);
    }
    out
}

const K: u64 = 30_000_000;
const EXPIRY_TICKS: u64 = 3; // the model's `Expiry`

fn word(st: &aterm_spec::interp::State) -> u64 {
    let latch = st["latch"] as u64;
    match (latch, st["seen"]) {
        (0, _) => 0,
        (l, 1) => (l * K - 1) | WAKE_LATCH_SENT,
        (l, _) => (l * K + 1) | WAKE_LATCH_SENT,
    }
}

fn idle_since(st: &aterm_spec::interp::State) -> u64 {
    if st["busy"] == 1 {
        0
    } else {
        st["drained"] as u64 * K
    }
}

#[test]
fn latch_conforms_to_damage_to_present_model() {
    let m = aterm_models_core::damage_to_present_model();
    const {
        assert!(K * EXPIRY_TICKS < WAKE_LATCH_EXPIRY_NS);
        assert!(K * (EXPIRY_TICKS + 1) - 1 > WAKE_LATCH_EXPIRY_NS);
    }
    let (mut checked, mut reposts) = (0_u32, 0_u32);
    let (mut duplicate_caught, mut stranded_caught) = (false, false);
    for st in reachable(&m) {
        let fresh = m.successors("DamageFresh", &st);
        let repost = m.successors("DamageRepost", &st);
        let model_posts = fresh.first().or(repost.first());
        let before = word(&st);
        let flag = AtomicU64::new(before);
        let now_ns = st["now"] as u64 * K;
        let mut sent = false;
        let gate = gated_output_wake(
            &flag,
            now_ns,
            idle_since(&st),
            false,
            || {
                sent = true;
                true
            },
            || now_ns + 1,
        );
        let expect_gate = if !fresh.is_empty() {
            WakeGate::Posted
        } else if !repost.is_empty() {
            reposts += 1;
            WakeGate::Reposted
        } else {
            WakeGate::Coalesced
        };
        assert_eq!(gate, expect_gate, "gated_output_wake vs model at {st:?}");
        assert_eq!(sent, model_posts.is_some(), "send vs model at {st:?}");
        let expect = model_posts.map_or(before, word);
        assert_eq!(flag.load(Ordering::Relaxed), expect, "latch word at {st:?}");
        let at = before & !WAKE_LATCH_SENT;
        let age_sends = before == 0 || now_ns.saturating_sub(at) > WAKE_LATCH_EXPIRY_NS;
        if age_sends && model_posts.is_none() && st["inflight"] == 1 {
            duplicate_caught = true;
        }
        if before != 0 && !repost.is_empty() {
            stranded_caught = true;
        }
        checked += 1;
    }
    assert!(
        checked > 10 && reposts > 0,
        "reachable-state walk degenerate ({checked}/{reposts})"
    );
    assert!(
        duplicate_caught,
        "the age re-send must disagree with the model on a queued wake, or this \
         bind cannot catch the 2026-09-29 duplicate wake"
    );
    assert!(
        stranded_caught,
        "the never-re-send reader must disagree with the model on a stranded wake"
    );
}

#[test]
fn watchdog_conforms_to_damage_to_present_model() {
    let m = aterm_models_core::damage_to_present_model();
    let (mut judged, mut idle_judged) = (0_u32, 0_u32);
    let (mut false_heal_caught, mut past_recheck_caught) = (false, false);
    let mut cut_heal_caught = false;
    for st in reachable(&m) {
        let (latch, now) = (st["latch"], st["now"] as u64 * K);
        let armed = word(&st);
        if st["busy"] == 1 {
            let drained = st["drained"] as u64 * K;
            // The bound stays 0: a latch the model holds through a cut turn was posted
            // after the bound the last emptied drain set.
            let mut watch = OutputWakeWatch {
                last_pass_ns: drained,
                ..OutputWakeWatch::default()
            };
            let word_now = AtomicU64::new(armed);
            let pass = watch.pass(now, st["cut"] == 0, [(1, &word_now)]);
            let end = m.successors("TurnEnd", &st);
            assert_eq!(end.len(), 1, "TurnEnd is deterministic");
            let end = &end[0];
            let model_heals = latch != 0 && end["latch"] == 0;
            assert_eq!(
                pass.healed == 1,
                model_heals,
                "OutputWakeWatch::pass vs model at {st:?}"
            );
            let drain_blind = judge_output_wake(armed, drained, now);
            if matches!(drain_blind, OutputWakeVerdict::Lost { .. })
                && !model_heals
                && st["inflight"] == 1
            {
                cut_heal_caught = true;
            }
            match pass.recheck_ns {
                Some(recheck_ns) => {
                    assert!(recheck_ns > now, "a recheck is never in the past");
                    assert_eq!(
                        recheck_ns.div_ceil(K),
                        end["recheck"] as u64,
                        "recheck deadline vs model at {st:?}"
                    );
                    let old = (armed & !WAKE_LATCH_SENT) + WAKE_LATCH_EXPIRY_NS + 1;
                    if old <= now && old.div_ceil(K) != end["recheck"] as u64 {
                        past_recheck_caught = true;
                    }
                }
                None => {
                    assert_eq!(end["recheck"], 0, "no deadline at {st:?}");
                }
            }
            let age_heals =
                latch != 0 && now.saturating_sub(armed & !WAKE_LATCH_SENT) > WAKE_LATCH_EXPIRY_NS;
            if age_heals && !model_heals && st["inflight"] == 1 {
                false_heal_caught = true;
            }
            judged += 1;
        } else if latch != 0 && st["inflight"] == 0 {
            let seen = st["seen"] == 1;
            assert_eq!(st["recheck"] != 0, seen, "recheck only if seen at {st:?}");
            assert_eq!(
                idle_since(&st) > (armed & !WAKE_LATCH_SENT),
                seen,
                "the idle stamp orders the arm as the model does at {st:?}"
            );
            idle_judged += 1;
        }
    }
    assert!(
        judged > 10 && idle_judged > 3,
        "walk degenerate ({judged}/{idle_judged})"
    );
    assert!(
        false_heal_caught,
        "the age-only verdict must disagree with the model on a queued wake, or \
         this bind cannot catch the 2026-09-29 false heal"
    );
    assert!(
        past_recheck_caught,
        "the stamp+expiry recheck must disagree with the model after a long frame"
    );
    assert!(
        cut_heal_caught,
        "a verdict blind to the drain must disagree with the model on a wake queued \
         behind a drain that stopped at a held lock, or this bind cannot catch the \
         2026-10-22 false heal"
    );
}

#[test]
fn a_300ms_frame_with_an_in_flight_wake_counts_no_heal() {
    let latch = AtomicU64::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watch = OutputWakeWatch::default();
    let loop_idle = LoopIdle::default();
    let t0 = 1_000 * MS;
    assert_eq!(watch.pass(t0, true, [(7, &latch)]).healed, 0);
    loop_idle.went_idle(t0);
    loop_idle.turn_began();
    assert!(rx.try_recv().is_err());
    for at in [10, 120, 200, 290] {
        gated_output_wake(
            &latch,
            t0 + at * MS,
            loop_idle.since_ns(),
            false,
            || tx.send(()).is_ok(),
            || t0 + at * MS,
        );
    }
    let end = t0 + 310 * MS;
    let pass = watch.pass(end, true, [(7, &latch)]);
    assert_eq!(
        pass.healed, 0,
        "a queued wake is LATE, not lost: no heal, no repaint-all"
    );
    assert_eq!(pass.late, 1, "booked once as late");
    assert_eq!(
        pass.recheck_ns,
        Some(end + WAKE_LATCH_EXPIRY_NS),
        "one expiry after this pass, not in its past"
    );
    assert_eq!(
        latch.load(Ordering::Relaxed),
        (t0 + 10 * MS) | WAKE_LATCH_SENT
    );
    assert!(end - (t0 + 10 * MS) > WAKE_LATCH_EXPIRY_NS);
    loop_idle.went_idle(end);
    let gate = gated_output_wake(
        &latch,
        end + MS,
        loop_idle.since_ns(),
        false,
        || tx.send(()).is_ok(),
        || end + MS,
    );
    assert_eq!(gate, WakeGate::Coalesced);
    assert_eq!(rx.try_iter().count(), 1, "no duplicate wake was posted");
    latch.store(0, Ordering::Relaxed);
    let next = watch.pass(end + 2 * MS, true, [(7, &latch)]);
    assert_eq!((next.healed, next.late, next.recheck_ns), (0, 0, None));
}

#[test]
fn consecutive_long_frames_never_arm_a_past_wake_heal_deadline() {
    use crate::deadline_heal::{DeadlineHealer, Heal};
    use crate::metrics::DeadlineOwner;
    use std::time::{Duration, Instant};
    let epoch = Instant::now();
    let at = |ns: u64| epoch + Duration::from_nanos(ns);
    let latch = AtomicU64::new(0);
    let mut watch = OutputWakeWatch::default();
    let mut healer = DeadlineHealer::default();
    let mut t = 1_000 * MS;
    assert_eq!(watch.pass(t, true, [(9, &latch)]).recheck_ns, None);
    for turn in 0..40 {
        latch.store(0, Ordering::Relaxed);
        let post = t + 5 * MS;
        assert_eq!(
            gated_output_wake(&latch, post, 0, false, || true, || post),
            WakeGate::Posted
        );
        t += 400 * MS;
        let pass = watch.pass(t, true, [(9, &latch)]);
        assert_eq!(pass.healed, 0, "turn {turn}: late, not lost");
        let recheck = pass
            .recheck_ns
            .expect("an outstanding wake folds a recheck");
        let (wanted, now) = (at(recheck), at(t + MS));
        assert!(
            wanted >= now,
            "turn {turn}: a recheck {} ms from the pass is a past arm",
            (i128::from(recheck) - i128::from(t)) / i128::from(MS)
        );
        assert_eq!(
            healer.heal(DeadlineOwner::WakeHeal, Some(wanted), now),
            Heal {
                armed: Some(wanted),
                floor: false,
                streak: false,
            },
            "turn {turn}: the recheck passes through unclamped"
        );
    }
}

#[test]
fn a_truly_lost_wake_still_heals_within_its_bound() {
    let latch = AtomicU64::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watch = OutputWakeWatch::default();
    let t0 = 1_000 * MS;
    watch.pass(t0, true, [(3, &latch)]);
    let sent = t0 + 10 * MS;
    gated_output_wake(&latch, sent, 0, false, || tx.send(()).is_ok(), || sent);
    assert_eq!(rx.try_iter().count(), 1);
    let pass_at = t0 + 12 * MS;
    let pass = watch.pass(pass_at, true, [(3, &latch)]);
    assert_eq!(
        pass.healed, 0,
        "not yet proven: no drain has begun after the post"
    );
    let recheck = pass
        .recheck_ns
        .expect("an armed latch asks the loop to look again");
    assert_eq!(
        recheck,
        pass_at + WAKE_LATCH_EXPIRY_NS,
        "one expiry after the pass that saw it"
    );
    let pass = watch.pass(recheck, true, [(3, &latch)]);
    assert_eq!(pass.healed, 1, "proven lost: healed");
    assert_eq!(latch.load(Ordering::Relaxed), 0);
    gated_output_wake(
        &latch,
        recheck + 1,
        0,
        false,
        || tx.send(()).is_ok(),
        || recheck + 1,
    );
    assert_eq!(rx.try_iter().count(), 1);
}

#[test]
fn a_wake_queued_behind_a_drain_that_stopped_short_is_not_healed() {
    let latch = AtomicU64::new(0);
    let mut watch = OutputWakeWatch::default();
    let t0 = 1_000 * MS;
    watch.pass(t0, true, [(0, &latch)]);
    let queue = std::cell::RefCell::new(std::collections::VecDeque::new());
    let sent = t0 + 3 * MS;
    let post = || {
        queue.borrow_mut().push_back(sent);
        true
    };
    assert_eq!(
        gated_output_wake(&latch, sent, 0, false, post, || sent),
        WakeGate::Posted,
        "posted after this turn's drain, during its frame"
    );
    assert_eq!(watch.pass(t0 + 4 * MS, true, [(0, &latch)]).healed, 0);
    let stopped = watch.pass(t0 + 5 * MS, false, [(0, &latch)]);
    assert_eq!(
        stopped.healed, 0,
        "the next drain met another sender mid-send and took nothing: queued, not lost"
    );
    assert_eq!(latch.load(Ordering::Relaxed), sent | WAKE_LATCH_SENT);
    assert!(stopped.recheck_ns.is_some(), "the watch looks again");
    assert_eq!(
        queue.borrow_mut().pop_front(),
        Some(sent),
        "the turn the sender's wake brings hands it out"
    );
    let handled = t0 + 6 * MS;
    let word = latch.load(Ordering::Relaxed);
    latch.store(output_latch_after_pass(word, handled), Ordering::Relaxed);
    let next = watch.pass(handled, true, [(0, &latch)]);
    assert_eq!((next.healed, next.recheck_ns), (0, None));
}

#[test]
fn a_wake_lost_around_a_drain_that_stopped_short_still_heals() {
    let (lost, stale) = (AtomicU64::new(0), AtomicU64::new(0));
    let mut watch = OutputWakeWatch::default();
    let t0 = 1_000 * MS;
    watch.pass(t0, true, [(0, &lost), (1, &stale)]);
    let sent = t0 + 3 * MS;
    gated_output_wake(&lost, sent, 0, false, || true, || sent);
    assert_eq!(watch.pass(t0 + 4 * MS, true, [(0, &lost)]).healed, 0);
    stale.store((t0 - MS) | WAKE_LATCH_SENT, Ordering::Relaxed);
    let stopped = watch.pass(t0 + 5 * MS, false, [(0, &lost), (1, &stale)]);
    assert_eq!(
        stopped.healed, 1,
        "a drain that stopped short keeps what an earlier emptied one proved"
    );
    assert_eq!(stale.load(Ordering::Relaxed), 0);
    assert_eq!(
        lost.load(Ordering::Relaxed),
        sent | WAKE_LATCH_SENT,
        "not proven by a drain that stopped short"
    );
    let pass = watch.pass(t0 + 6 * MS, true, [(0, &lost)]);
    assert_eq!(pass.healed, 1, "the next emptied drain proves it lost");
    assert_eq!(lost.load(Ordering::Relaxed), 0);
}

#[test]
fn an_unsent_arm_is_never_presumed_lost() {
    let v = judge_output_wake(5 * MS, u64::MAX, 900 * MS);
    assert_eq!(
        v,
        OutputWakeVerdict::Waiting {
            recheck_ns: 900 * MS + 895 * MS,
            late: true
        }
    );
    let young = judge_output_wake(5 * MS, u64::MAX, 6 * MS);
    assert_eq!(
        young,
        OutputWakeVerdict::Waiting {
            recheck_ns: 6 * MS + WAKE_LATCH_EXPIRY_NS,
            late: false
        }
    );
    assert_eq!(
        judge_output_wake(0, u64::MAX, 900 * MS),
        OutputWakeVerdict::Clear
    );
    assert!(
        !wake_stranded(5 * MS, 1, 900 * MS),
        "unsent: never stranded"
    );
    assert!(wake_stranded((5 * MS) | WAKE_LATCH_SENT, 1, 900 * MS));
}

#[test]
fn a_late_wake_is_booked_once() {
    let latch = AtomicU64::new((10 * MS) | WAKE_LATCH_SENT);
    let mut watch = OutputWakeWatch::default();
    let mut books = 0;
    for _ in 0..3 {
        watch.last_pass_ns = 10 * MS;
        books += watch
            .pass(10 * MS + 2 * WAKE_LATCH_EXPIRY_NS, true, [(1, &latch)])
            .late;
    }
    assert_eq!(books, 1);
    latch.store((20 * MS) | WAKE_LATCH_SENT, Ordering::Relaxed);
    watch.last_pass_ns = 20 * MS;
    assert_eq!(
        watch
            .pass(20 * MS + 2 * WAKE_LATCH_EXPIRY_NS, true, [(1, &latch)])
            .late,
        1
    );
}
