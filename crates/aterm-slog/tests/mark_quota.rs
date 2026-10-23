// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the mark quota and per-author mark rate limiter.

use aterm_slog::{
    Actor, Conf, Fields, Key, Kind, Limits, Log, MARK_ACTORS, MARK_BURST, MARK_OWED_AUTHORS,
    MARK_REFILL_MS, Mark, MarkLimiter, Node, Owed, ShortId, Src, mark_quota,
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

#[test]
fn seventeen_actors_share_the_overflow() {
    let mut lim = MarkLimiter::new(0);
    for i in 0..MARK_ACTORS {
        lim.spend(&minted(i), 0);
    }
    for _ in 0..MARK_BURST {
        lim.spend(&minted(100), 0);
    }
    assert!(lim.peek(&minted(101), 0).is_err(), "the overflow is shared");
    assert!(lim.peek(&minted(3), 0).is_ok());
}

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

#[test]
fn a_short_count_rides_only_its_own_bucket() {
    let mut lim = MarkLimiter::new(0);
    let (a, b) = (minted(1), minted(2));
    for _ in 0..MARK_BURST {
        lim.spend(&a, 0);
    }
    lim.spend(&b, 0);
    for _ in 0..79 {
        assert!(lim.peek(&a, 0).is_err(), "A is short");
        lim.note_short(&a, 0);
    }
    assert_eq!(lim.owed(&b, 0), Owed::Own(0), "B was short of nothing");
    assert_eq!(
        lim.owed(&minted(3), 0),
        Owed::Own(0),
        "a session with no slot yet"
    );
    assert_eq!(lim.owed(&a, 0), Owed::Own(79));
    lim.said(&b, 0);
    assert_eq!(
        lim.owed(&a, 0),
        Owed::Own(79),
        "B saying its own leaves A's"
    );
    lim.said(&a, 0);
    assert_eq!(lim.owed(&a, 0), Owed::Own(0), "said once");
    for _ in 0..3 {
        lim.note_short(&Actor::Nobody, 0);
    }
    assert_eq!(
        lim.owed(&Actor::Aterm, 0),
        Owed::Shared(0),
        "the unattributed author's count is its own, not aterm's"
    );
    assert_eq!(lim.owed(&Actor::Nobody, 0), Owed::Own(3));
    lim.said(&Actor::Vendor, 0);
    assert_eq!(
        lim.owed(&Actor::Nobody, 0),
        Owed::Own(3),
        "the vendor saying the pool leaves it"
    );
    lim.said(&Actor::Nobody, 0);
    assert_eq!(lim.owed(&Actor::Nobody, 0), Owed::Shared(0), "said once");
    assert_eq!(lim.owed(&Actor::Harness, 0), Owed::Own(0));
    lim.note_short(&Actor::Harness, 0);
    assert_eq!(lim.owed(&Actor::Harness, 0), Owed::Own(1));
    assert_eq!(
        lim.owed(&Actor::Ctl, 0),
        Owed::Own(0),
        "the harness's is its own"
    );
    lim.note_short(&a, 0);
    for i in 4..MARK_ACTORS + 2 {
        lim.spend(&minted(i), 0);
    }
    let idle = MARK_REFILL_MS * u64::from(MARK_BURST) * 2;
    for i in 100..100 + MARK_ACTORS - 1 {
        lim.spend(&minted(i), idle);
    }
    assert_eq!(
        lim.owed(&a, idle),
        Owed::Own(1),
        "an idle slot owing nothing went first"
    );
    lim.spend(&minted(500), idle);
    assert_eq!(
        lim.owed(&a, idle),
        Owed::Own(1),
        "the last slot went, its count with A"
    );
    assert_eq!(
        lim.owed(&minted(500), idle),
        Owed::Own(0),
        "the slot's new holder owes none of A's count"
    );

    let mut lim = MarkLimiter::new(0);
    let (a, fresh) = (minted(1), minted(7));
    for _ in 0..3 {
        lim.note_short(&Actor::Nobody, 0);
    }
    lim.spend(&a, 0);
    assert_eq!(
        lim.owed(&fresh, 0),
        Owed::Own(0),
        "a free slot owes nothing"
    );
    lim.said(&fresh, 0);
    assert_eq!(
        lim.owed(&Actor::Nobody, 0),
        Owed::Own(3),
        "the unattributed author's still owed"
    );
    for _ in 0..MARK_BURST - 1 {
        lim.spend(&a, 0);
    }
    for _ in 0..5 {
        lim.note_short(&a, 0);
    }
    let idle = MARK_REFILL_MS * u64::from(MARK_BURST) * 2;
    for i in 100..100 + MARK_ACTORS - 1 {
        lim.spend(&minted(i), idle);
    }
    assert!(
        lim.peek(&fresh, idle).is_ok(),
        "A's idle slot is there to take"
    );
    assert_eq!(
        lim.owed(&fresh, idle),
        Owed::Own(0),
        "A's count is not the new session's"
    );
    lim.said(&fresh, idle);
    assert_eq!(
        lim.owed(&a, idle),
        Owed::Own(5),
        "nor did the new session clear it"
    );
    lim.spend(&fresh, idle);
    assert_eq!(
        lim.owed(&fresh, idle),
        Owed::Own(0),
        "the slot it took owes nothing"
    );
    assert_eq!(
        lim.owed(&a, idle),
        Owed::Own(5),
        "A's 5 went with A, past the table"
    );
    let later = idle * 2;
    lim.spend(&a, later);
    assert_eq!(lim.owed(&a, later), Owed::Own(5), "brought into its slot");
    lim.said(&a, later);
    assert_eq!(lim.owed(&a, later), Owed::Own(0), "said once");
}

#[test]
fn an_overflowed_authors_count_is_its_own_and_past_the_table_the_pools() {
    let mut lim = MarkLimiter::new(0);
    for i in 0..MARK_ACTORS {
        for _ in 0..MARK_BURST {
            lim.spend(&minted(i), 0);
        }
    }
    for _ in 0..MARK_BURST {
        lim.spend(&minted(1000), 0);
    }
    let past: Vec<Actor> = (0..=MARK_OWED_AUTHORS).map(|i| minted(2000 + i)).collect();
    for (k, by) in past.iter().enumerate() {
        for _ in 0..=k {
            assert!(lim.peek(by, 0).is_err(), "the overflow's tokens are spent");
            lim.note_short(by, 0);
        }
    }
    assert_eq!(
        lim.owed(&past[0], 0),
        Owed::Shared(1),
        "the least recently noted spilled into the pool"
    );
    for (k, by) in past.iter().enumerate().skip(1) {
        let own = u64::try_from(k + 1).expect("small");
        assert_eq!(lim.owed(by, 0), Owed::Own(own), "author {k}");
    }
    lim.note_short(&past[1], 0);
    lim.note_short(&minted(3000), 0);
    assert_eq!(lim.owed(&past[1], 0), Owed::Own(3));
    assert_eq!(lim.owed(&past[2], 0), Owed::Shared(1 + 3));
    assert_eq!(lim.owed(&minted(3000), 0), Owed::Own(1));
    lim.said(&past[1], 0);
    assert_eq!(lim.owed(&past[1], 0), Owed::Shared(4), "its own was said");
    lim.said(&past[0], 0);
    assert_eq!(lim.owed(&past[2], 0), Owed::Shared(0), "the pool was said");
    assert_eq!(lim.owed(&past[3], 0), Owed::Own(4), "its own stands");
    for (n, by) in [Actor::Aterm, Actor::Vendor, Actor::Nobody]
        .iter()
        .enumerate()
    {
        for _ in 0..=n {
            lim.note_short(by, 0);
        }
    }
    for (n, by) in [Actor::Aterm, Actor::Vendor, Actor::Nobody]
        .iter()
        .enumerate()
    {
        let own = u64::try_from(n + 1).expect("small");
        assert_eq!(lim.owed(by, 0), Owed::Own(own), "{by:?}");
    }
    assert!(lim.peek(&minted(4000), 0).is_err());
    assert!(lim.peek(&minted(4000), MARK_REFILL_MS).is_ok());
}

#[test]
fn a_session_taking_a_slot_from_a_full_table_brings_its_own_count() {
    let mut lim = MarkLimiter::new(0);
    for i in 0..MARK_ACTORS {
        for _ in 0..MARK_BURST {
            lim.spend(&minted(i), 0);
        }
        lim.note_short(&minted(i), 0);
    }
    for _ in 0..MARK_BURST {
        lim.spend(&minted(1000), 0);
    }
    let past: Vec<Actor> = (0..MARK_OWED_AUTHORS).map(|i| minted(2000 + i)).collect();
    for by in &past {
        assert!(lim.peek(by, 0).is_err(), "the overflow's tokens are spent");
        lim.note_short(by, 0);
    }
    assert_eq!(lim.owed(&past[0], 0), Owed::Own(1), "the table is full");
    assert_eq!(
        lim.owed(&Actor::Aterm, 0),
        Owed::Shared(0),
        "nothing spilled"
    );
    let later = MARK_REFILL_MS * u64::from(MARK_BURST) * 2;
    lim.spend(&past[0], later);
    assert_eq!(
        lim.owed(&past[0], later),
        Owed::Own(1),
        "its own count came into the slot"
    );
    assert_eq!(
        lim.owed(&minted(0), later),
        Owed::Own(1),
        "the displaced holder's went with it"
    );
    assert_eq!(
        lim.owed(&Actor::Aterm, later),
        Owed::Shared(0),
        "and nothing spilled into the pool"
    );
}
