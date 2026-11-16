// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JUDGMENT TURN ([`crate::Heavy`], `docs/DESIGN-landing-economy.md` §2.3): heavy work takes
//! it one claim at a time, shortest predicted first and aged by its wait, and its holder's own
//! claims of cores are granted all the room.

use super::*;

const MIN: u64 = 60_000;

fn record(pid: u32, start_ms: u64, need: Need, held: Option<u32>) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid,
        start_ms,
        laid_at: None,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: held.map(|cores| crate::Held {
            admitted_ms: start_ms,
            cores,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

fn other(record: &Record) -> Other<'_> {
    Other {
        record,
        used: TreeUse::default(),
        credited: TreeUse::default(),
        seen: true,
        line: Vec::new(),
        sole: true,
        inside: None,
        reckoned: None,
    }
}

/// A claim of the turn queued at `since_ms`, predicted to take `predicted_ms` of the machine.
fn turn(what: &str, since_ms: u64, predicted_ms: u64) -> Need {
    Need::new(what).heavy(since_ms, predicted_ms)
}

fn idle() -> Reading {
    Reading::of_gib(48, 40, 18)
}

fn heavy_short(result: &Result<Grant, Shortfall>) -> Option<(u32, bool)> {
    result.as_ref().err().and_then(|s| {
        s.shorts.iter().find_map(|short| match short {
            Short::Heavy { pid, held, .. } => Some((*pid, *held)),
            _ => None,
        })
    })
}

/// Of two heavy waiters, the one whose key (queued since plus predicted) is smaller takes the
/// turn, whichever arrived first, and the other waits for it, naming it; the shortfall's holders
/// name it too. NEGATIVE CONTROL: the one with the smaller key is admitted though it arrived
/// after the other.
#[test]
fn of_two_heavy_waiters_the_smaller_key_takes_the_turn() {
    let long = record(10, 1, turn("verify judgment turn: long", 0, 40 * MIN), None);
    let short = record(11, 2, turn("verify judgment turn: short", 0, 5 * MIN), None);
    let mine_long = (long.start_ms, long.pid, long.seq);
    let mine_short = (short.start_ms, short.pid, short.seq);
    let waits = decide(&long.need, mine_long, &idle(), &[other(&short)], 10);
    assert_eq!(heavy_short(&waits), Some((11, false)));
    let shortfall = waits.unwrap_err();
    assert!(
        shortfall.holders.iter().any(|h| h.pid == 11),
        "{shortfall:?}"
    );
    let text = shortfall.shorts[0].to_string();
    assert!(
        text.contains("the judgment turn") && text.contains("comes first"),
        "{text}"
    );
    assert!(
        decide(&short.need, mine_short, &idle(), &[other(&long)], 10).is_ok(),
        "the shorter one is admitted though it came second"
    );
}

/// AGING: a key is when the work was queued plus its prediction, so a long run's wait counts
/// against shorter work that arrives later. A at 0 predicted 60 minutes has key 60; B queued at
/// 50 predicted 5 (key 55) comes first; C queued at 56 predicted 5 (key 61) waits behind A.
#[test]
fn a_long_waiter_ages_ahead_of_short_work_that_arrives_late_enough() {
    let a = record(10, 0, turn("a", 0, 60 * MIN), None);
    let b = record(11, 50 * MIN, turn("b", 50 * MIN, 5 * MIN), None);
    let c = record(12, 56 * MIN, turn("c", 56 * MIN, 5 * MIN), None);
    let now = 56 * MIN;
    let at = |r: &Record| (r.start_ms, r.pid, r.seq);
    assert!(decide(&b.need, at(&b), &idle(), &[other(&a), other(&c)], now).is_ok());
    assert_eq!(
        heavy_short(&decide(
            &a.need,
            at(&a),
            &idle(),
            &[other(&b), other(&c)],
            now
        )),
        Some((11, false))
    );
    assert_eq!(
        heavy_short(&decide(&c.need, at(&c), &idle(), &[other(&a)], now)),
        Some((10, false)),
        "C's key is past A's"
    );
    let queue: Vec<u32> = heavy_queue(&[c.clone(), a.clone(), b.clone()])
        .iter()
        .map(|r| r.pid)
        .collect();
    assert_eq!(queue, [11, 10, 12]);
}

/// A held turn blocks every heavy waiter, whatever its key, while claims that ask nothing of the
/// turn (memory, cores) are admitted beside it; a claim of the holder's own process is never held
/// back by its own turn. The queue lists the holder first.
#[test]
fn a_held_turn_blocks_heavy_waiters_and_nothing_else() {
    let holder = record(10, 0, turn("held", 0, 60 * MIN), Some(0));
    let tiny = record(11, 1, turn("tiny", MIN, MIN), None);
    let at = |r: &Record| (r.start_ms, r.pid, r.seq);
    assert_eq!(
        heavy_short(&decide(
            &tiny.need,
            at(&tiny),
            &idle(),
            &[other(&holder)],
            2
        )),
        Some((10, true)),
        "a smaller key does not take a held turn"
    );
    let text = decide(&tiny.need, at(&tiny), &idle(), &[other(&holder)], 2)
        .unwrap_err()
        .shorts[0]
        .to_string();
    assert!(text.contains("holds it"), "{text}");
    let build = Need::new("cargo build").mem(4 * GIB).cores(4);
    assert!(decide(&build, (3, 12, 0), &idle(), &[other(&holder)], 4).is_ok());
    let own = turn("own second", 0, 60 * MIN);
    assert!(
        decide(&own, (5, 10, 1), &idle(), &[other(&holder)], 6).is_ok(),
        "a process does not wait for its own turn"
    );
    let queue: Vec<u32> = heavy_queue(&[tiny.clone(), holder.clone()])
        .iter()
        .map(|r| r.pid)
        .collect();
    assert_eq!(queue, [10, 11]);
}

/// CORES-GRANT: on 18 cores at a 5-minute load of 46.6, with another process holding 5 cores
/// it was granted long ago, a claim of the turn's holder is granted all 13 cores left, where the
/// same claim of a process that does not hold it gets the fair share, 4.
#[test]
fn the_turns_holder_is_granted_all_the_room() {
    let reading = Reading {
        load1: Some(46.6),
        load5: Some(46.6),
        ..idle()
    };
    let now = 10 * 60 * MIN;
    let busy = record(20, 0, Need::new("cargo test").cores(5), Some(5));
    let holder = record(10, 1, turn("held", 0, 30 * MIN), Some(0));
    let tests = Need::new("verify test subprocesses").cores(18);
    let granted = decide(
        &tests,
        (now, 10, 2),
        &reading,
        &[other(&busy), other(&holder)],
        now,
    )
    .expect("room")
    .cores;
    assert_eq!(granted, 13);
    let fair = decide(&tests, (now, 30, 0), &reading, &[other(&busy)], now)
        .expect("room")
        .cores;
    assert_eq!(fair, 4, "NEGATIVE CONTROL: a process without the turn");
    let child = decide(
        &tests,
        (now, 31, 0),
        &reading,
        &[other(&busy), other(&holder)],
        now,
    )
    .expect("room")
    .cores;
    assert_eq!(child, 4, "another process's claim keeps the fair share");
}

/// The turn is Ordinary work's: a Release or Background claim that asks for it can never be
/// admitted. NEGATIVE CONTROL: the same claim at Ordinary rank can.
#[test]
fn only_ordinary_work_asks_for_the_turn() {
    let reading = idle();
    assert!(never(&turn("ordinary", 0, MIN), &reading).is_none());
    for rank in [crate::Rank::Release, crate::Rank::Background] {
        let mut need = turn("ranked", 0, MIN);
        need.rank = rank;
        let why = never(&need, &reading).expect("never");
        assert!(why.contains("judgment turn"), "{why}");
    }
}
