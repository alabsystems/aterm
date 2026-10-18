// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mark quota and the mark limiter (design §7.4, phase 3A; rulings R4,
//! R11): claims hold at most a quarter of a ring, taken oldest first and
//! never an observation or a fenced record; one author marks at most its
//! burst plus one a second, and the trusted authors' buckets are their own.

use aterm_slog::{
    Actor, Conf, Fields, Key, Kind, Limits, Log, MARK_ACTORS, MARK_BURST, MARK_REFILL_MS, Mark,
    MarkLimiter, Node, ShortId, Src, mark_quota,
};
use aterm_spec::rng::XorShift;

use crate::support::at;

fn claim(log: &mut Log, ms: u64, text: &str) -> u64 {
    log.mark(
        Mark::new(Kind::Note, Src::Mark, Conf::Claim)
            .by(Actor::Ctl)
            .fields(Fields::new().text(Key::Text, text).expect("text")),
        at(ms),
    )
    .expect("a claim")
}

fn observe(log: &mut Log, ms: u64) -> u64 {
    log.mark(
        Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(Node::SESSION),
        at(ms),
    )
    .expect("an observation")
}

/// 10⁴ claims among 10³ observations at the session bound: at every step the
/// claims hold at most a quarter of the ring's records and bytes; NEGATIVE:
/// no observation was taken, and every claim taken is counted lost.
#[test]
fn marks_never_pass_a_quarter_of_capacity() {
    let mut log = Log::new(1);
    let q = mark_quota(Limits::SESSION);
    assert_eq!((q.records, q.bytes), (512, 64 * 1024));
    let mut rng = XorShift::new(7);
    let (mut claims, mut observations) = (0u64, 0u64);
    let text = "x".repeat(150);
    for i in 0..11_000u64 {
        if rng.below(11) == 0 && observations < 1_000 {
            observe(&mut log, i);
            observations += 1;
        } else {
            claim(
                &mut log,
                i,
                &text[..usize::try_from(i % 150).unwrap_or(0) + 1],
            );
            claims += 1;
        }
        let (n, bytes) = log.marks_held();
        assert!(n <= q.records && bytes <= q.bytes, "{n} marks, {bytes} B");
    }
    let live_obs = log.window().filter(|r| r.src != Src::Mark).count() as u64;
    let (live_marks, _) = log.marks_held();
    assert_eq!(live_obs, observations, "the quota took no observation");
    assert_eq!(
        log.lost(),
        claims - live_marks as u64,
        "every claim taken counted"
    );
}

/// A lowered byte budget lowers the claims' share at once.
#[test]
fn the_quota_follows_a_lowered_budget() {
    let mut log = Log::new(1);
    for i in 0..400 {
        claim(&mut log, i, "a claim of some forty bytes, give or take");
    }
    let before = log.marks_held();
    log.set_byte_budget(16 * 1024);
    let after = log.marks_held();
    assert!(
        after.1 <= 4 * 1024 && after.0 < before.0,
        "{before:?} -> {after:?}"
    );
    assert_eq!(log.lost(), 400 - after.0 as u64);
}

/// R4: while the readers are fenced (an update's overlap), the quota takes
/// no record at or below the fence: claims pushed far past the quota leave
/// every fenced claim live and take only the newer ones (the newest always
/// stands); once the fence lifts, the quota holds again.
#[test]
fn the_quota_never_takes_a_fenced_mark() {
    let mut log = Log::with_limits(1, Limits::new(64, 1 << 20).expect("l"));
    let q = mark_quota(log.effective_limits());
    let mut fenced = Vec::new();
    for i in 0..q.records as u64 {
        fenced.push(claim(&mut log, i, "before the carry"));
    }
    log.fence_reads();
    let fence = log.read_fence().expect("fenced");
    for i in 0..40u64 {
        claim(&mut log, 100 + i, "during the overlap");
        let live: Vec<u64> = log.window().map(|r| r.seq).collect();
        assert!(
            fenced.iter().all(|s| live.contains(s)),
            "a fenced claim taken"
        );
        let past = log
            .window()
            .filter(|r| r.src == Src::Mark && r.seq > fence)
            .count();
        assert_eq!(past, 1, "only the newest beside the fenced ones");
    }
    log.lift_read_fence();
    let (n, _) = log.marks_held();
    assert!(n <= q.records, "{n} after the fence lifted");
}

/// The limiter admits, from one author, at most its burst plus one per
/// second elapsed: a property over random arrival times.
#[test]
fn the_limiter_admits_at_most_burst_plus_elapsed() {
    for seed in 1..=50u64 {
        let mut rng = XorShift::new(seed);
        let mut lim = MarkLimiter::new(0);
        let by = Actor::Ctl;
        let mut now = 0u64;
        let mut admitted = 0u64;
        for _ in 0..500 {
            now += rng.below(400);
            if lim.peek(&by, now).is_ok() {
                lim.spend(&by, now);
                admitted += 1;
            }
            assert!(
                admitted <= u64::from(MARK_BURST) + now / MARK_REFILL_MS,
                "seed {seed}: {admitted} by {now} ms"
            );
        }
    }
}

/// A refused mark learns when to retry, and at that time it is admitted.
#[test]
fn a_refused_mark_learns_when_to_retry() {
    let mut lim = MarkLimiter::new(0);
    let by = Actor::Harness;
    for _ in 0..MARK_BURST {
        assert!(lim.peek(&by, 10).is_ok());
        lim.spend(&by, 10);
    }
    let retry = lim.peek(&by, 10).expect_err("the twenty-first is refused");
    assert!((1..=MARK_REFILL_MS).contains(&retry), "{retry}");
    assert!(lim.peek(&by, 10 + retry - 1).is_err(), "not before");
    assert!(lim.peek(&by, 10 + retry).is_ok(), "at retry_ms");
}

fn minted(i: usize) -> Actor {
    Actor::Sid(ShortId::new(&format!("s-{i:020x}")).expect("sid"))
}

/// The seventeenth and later sessions share the overflow bucket; a named
/// slot in debt is never given to another session.
#[test]
fn seventeen_actors_share_the_overflow() {
    let mut lim = MarkLimiter::new(0);
    for i in 0..MARK_ACTORS {
        lim.spend(&minted(i), 0);
    }
    // Two sessions past the table share one bucket.
    for _ in 0..MARK_BURST {
        lim.spend(&minted(100), 0);
    }
    assert!(lim.peek(&minted(101), 0).is_err(), "the overflow is shared");
    // Each named session still has its own nineteen.
    assert!(lim.peek(&minted(3), 0).is_ok());
}

/// R11: a flood of 64 minted sessions, each spending its whole burst, never
/// delays the harness, the bridge, the owner's clients or the person.
#[test]
fn a_flood_of_minted_edges_never_delays_a_trusted_author() {
    let mut lim = MarkLimiter::new(0);
    for i in 0..64 {
        for _ in 0..MARK_BURST {
            if lim.peek(&minted(i), 5).is_ok() {
                lim.spend(&minted(i), 5);
            }
        }
    }
    for by in [Actor::Harness, Actor::Bridge, Actor::Ctl, Actor::Human] {
        for _ in 0..MARK_BURST {
            assert!(lim.peek(&by, 5).is_ok(), "{by:?} delayed by the flood");
            lim.spend(&by, 5);
        }
    }
}
