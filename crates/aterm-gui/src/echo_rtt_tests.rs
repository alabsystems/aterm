// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fresh() -> std::sync::MutexGuard<'static, ()> {
    fresh_for_test()
}

/// Tests of one binary let their own sinks through at once, each guard ending with its test:
/// one guard's end must leave another test's sink let through. One slot the guard clears
/// fails here. (Addresses 1 and 2 are no sink's, which are aligned allocations.)
#[test]
fn a_guard_ending_beside_another_keeps_the_other_sink_let_through() {
    let first = let_through(1);
    let second = let_through(2);
    drop(first);
    assert!(reaches(2), "one guard's end shut another test's sink out");
    assert!(!reaches(1), "an ended guard still lets its sink through");
    drop(second);
    assert!(!reaches(2));
}

/// The basic contract: a write arms, that session's next burst closes, and
/// the recorded number is the interval between them.
#[test]
fn a_write_then_its_sessions_burst_records_the_interval() {
    let _g = fresh();
    arm_at(7, 1_000);
    assert_eq!(close_at(7, 6_000), Some(5_000));
    let s = snapshot();
    assert_eq!((s.n, s.total, s.arms), (1, 1, 1));
    assert_eq!((s.p50_us, s.p95_us, s.p99_us), (5_000, 5_000, 5_000));
    assert_eq!(s.expired, 0, "an answered write is not an expiry");
}

/// Another session's output cannot close the clock, or a background flood would answer a
/// keystroke typed into a quiet pane.
#[test]
fn another_sessions_output_cannot_close_the_clock() {
    let _g = fresh();
    arm_at(7, 1_000);
    assert_eq!(close_at(8, 2_000), None);
    assert_eq!(snapshot().total, 0);
    assert_eq!(
        close_at(7, 3_000),
        Some(2_000),
        "the real session still can"
    );
}

/// With nothing armed, output records nothing: a pure `cat`/`yes` flood stays out.
#[test]
fn output_with_no_outstanding_write_records_nothing() {
    let _g = fresh();
    for t in 0..1_000 {
        note_output_burst(7);
        assert_eq!(close_at(7, t), None);
    }
    assert_eq!(snapshot().total, 0);
}

/// Keep-oldest: a second key while one is outstanding does not re-arm, and is counted.
#[test]
fn a_second_key_coalesces_and_the_bias_is_published() {
    let _g = fresh();
    arm_at(7, 1_000);
    arm_at(7, 2_000);
    arm_at(7, 3_000);
    assert_eq!(
        close_at(7, 11_000),
        Some(10_000),
        "measured from the OLDEST unanswered write"
    );
    let s = snapshot();
    assert_eq!((s.arms, s.coalesced), (1, 2));
}

/// Past the TTL the next keystroke takes the slot and the unanswered write is booked as
/// expired, never as a sample.
#[test]
fn an_unanswered_write_expires_and_frees_the_slot() {
    let _g = fresh();
    arm_at(7, 1_000);
    arm_at(7, 1_000 + ARM_TTL_US);
    let s = snapshot();
    assert_eq!((s.expired, s.arms, s.total), (1, 2, 0));
    assert_eq!(
        close_at(7, 1_000 + ARM_TTL_US + 42),
        Some(42),
        "the NEW write owns the slot"
    );
}

/// A session no other test's reader can be: its tag is not a small id.
const QUIET: u64 = 0xBEE7;

/// The echo came back while the dispatch that wrote it was still running, before the arm:
/// it is the sample, measured from where the write began, and the session's next output
/// (say, the next keystroke's echo half a second later) answers nothing.
#[test]
fn an_answer_that_beat_the_arm_is_its_sample_not_the_next_output() {
    let _g = fresh();
    note_output_burst_at(QUIET, 1_400);
    arm_catching(QUIET, 1_000);
    let s = snapshot();
    assert_eq!((s.arms, s.total, s.last_us), (1, 1, 400), "{s:?}");
    note_output_burst_at(QUIET, 500_000);
    assert_eq!(snapshot().total, 1, "nothing was left armed for it");
}

/// Output from before the write began is no answer to it: the clock stays armed for the
/// session's next output.
#[test]
fn output_from_before_the_write_is_no_answer() {
    let _g = fresh();
    note_output_burst_at(QUIET, 900);
    arm_catching(QUIET, 1_000);
    assert_eq!(snapshot().total, 0);
    assert_eq!(close_at(QUIET, 1_250), Some(250));
}

/// A session that shares the burst slot cannot answer for this one.
#[test]
fn a_slot_sharing_sessions_output_is_not_this_sessions_answer() {
    let _g = fresh();
    let other = QUIET + BURST_SLOTS as u64;
    assert!(std::ptr::eq(burst_slot(other), burst_slot(QUIET)));
    note_output_burst_at(other, 1_400);
    arm_catching(QUIET, 1_000);
    assert_eq!(snapshot().total, 0);
    assert_eq!(
        close_at(QUIET, 1_600),
        Some(600),
        "its own output still does"
    );
}

/// A metrics reset forgets the bursts too, so an old one cannot answer a new write.
#[test]
fn a_reset_forgets_the_last_bursts() {
    let _g = fresh();
    note_output_burst_at(QUIET, 9_000);
    reset();
    arm_catching(QUIET, 1_000);
    assert_eq!(snapshot().total, 0);
}

/// A thread's write start is its first write's since it was forgotten.
#[test]
fn a_write_start_is_the_first_writes_until_forgotten() {
    forget_write_start();
    assert_eq!(take_write_start(), None);
    note_write_start();
    let first = WRITE_START_US.with(std::cell::Cell::get);
    note_write_start();
    assert_eq!(
        take_write_start(),
        Some(first),
        "a later write keeps the first"
    );
    assert_eq!(take_write_start(), None, "taking it clears it");
    note_write_start();
    forget_write_start();
    assert_eq!(take_write_start(), None);
}

/// A slow echo is recorded at its true length, not truncated.
#[test]
fn a_very_slow_echo_is_recorded_not_truncated() {
    let _g = fresh();
    arm_at(7, 1_000);
    let slow = ARM_TTL_US * 3;
    assert_eq!(close_at(7, 1_000 + slow), Some(slow));
    assert_eq!(snapshot().max_us, slow);
}

/// Percentiles are exact order statistics over the trailing window, and the
/// window rolls: 1..=WINDOW+500 leaves the oldest 500 out of the p50.
#[test]
fn percentiles_are_exact_over_the_trailing_window() {
    let _g = fresh();
    for i in 1..=100u64 {
        arm_at(7, 1);
        assert_eq!(close_at(7, 1 + i), Some(i));
    }
    let s = snapshot();
    assert_eq!((s.n, s.total), (100, 100));
    assert_eq!((s.p50_us, s.p95_us, s.p99_us), (50, 95, 99));

    for i in 101..=(WINDOW as u64 + 500) {
        arm_at(7, 1);
        assert_eq!(close_at(7, 1 + i), Some(i));
    }
    let s = snapshot();
    assert_eq!(s.n, WINDOW as u64, "the window is bounded");
    assert_eq!(s.total, WINDOW as u64 + 500, "the all-time count is not");
    assert_eq!(s.p50_us, 501 + 511);
}

/// The nearest rank written as its definition, the smallest sample with at
/// least `q·n` samples at or below it, rather than as the `ceil(q·n) - 1`
/// index the production walk uses.
fn nearest_rank(sorted: &[u32], q: f64) -> u64 {
    let need = q * sorted.len() as f64;
    let i = (0..sorted.len())
        .find(|&i| (i + 1) as f64 >= need)
        .expect("q <= 1 always reaches a rank");
    u64::from(sorted[i])
}

/// A copied window's percentiles equal a sorted reference over exactly the
/// samples the window holds: one sample, two, an odd length, a full window,
/// past the wrap, and refilled after a clear that left the old samples in
/// the array beyond `len`.
#[test]
fn copied_window_percentiles_match_a_sorted_reference() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut sample = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % 50_000
    };
    let check = |ring: &Ring, held: &[u64], case: &str| {
        let mut sorted: Vec<u32> = held
            .iter()
            .map(|&us| u32::try_from(us).expect("samples fit the ring's u32"))
            .collect();
        sorted.sort_unstable();
        let mut copy = ring.copy_out();
        assert_eq!(copy.len, held.len(), "{case}: the copy holds the window");
        let want = (
            nearest_rank(&sorted, 0.50),
            nearest_rank(&sorted, 0.95),
            nearest_rank(&sorted, 0.99),
        );
        assert_eq!(copy.percentiles(), want, "{case}");
    };
    for n in [1, 2, 63, WINDOW, WINDOW + 37] {
        let mut ring = Ring::new();
        let pushed: Vec<u64> = (0..n).map(|_| sample()).collect();
        for &us in &pushed {
            ring.push(us);
        }
        check(
            &ring,
            &pushed[n.saturating_sub(WINDOW)..],
            &format!("n={n}"),
        );
    }
    let mut ring = Ring::new();
    for _ in 0..WINDOW {
        ring.push(1_000_000 + sample());
    }
    ring.clear();
    let refill: Vec<u64> = (0..5).map(|_| sample()).collect();
    for &us in &refill {
        ring.push(us);
    }
    check(&ring, &refill, "refilled after a clear");

    let mut two = Ring::new();
    two.push(20);
    two.push(10);
    assert_eq!(two.copy_out().percentiles(), (10, 20, 20));
    let off_by_one = u64::from([10_u32, 20][(0.50_f64 * 2.0).ceil() as usize]);
    assert_ne!(off_by_one, nearest_rank(&[10, 20], 0.50));
}
