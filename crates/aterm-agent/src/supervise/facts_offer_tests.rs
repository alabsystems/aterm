// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A proven self-claim's facts account: each stalled call offered to one reader at a time,
//! leased, taken again by its own reader, and told once, on its ack alone.

use super::*;
use crate::supervise::BackgroundCall;
use crate::supervise::facts::FACT_FRESH_MS;

const T0: i64 = 1_760_000_000_000;

/// The claim's proof: one codex-alab process.
const CLAIM: &str = "codex-alab@4242 started Mon";

/// Another process's proof (a relaunch, or another claimant).
const OTHER: &str = "codex-alab@5151 started Tue";

/// The claim's two root threads.
const R1: &str = "thread-1";
const R2: &str = "thread-2";

/// A call at no progress whose leader `pid` leads its group, quiet `min` minutes.
fn call(pid: u32, min: u64) -> BackgroundCall {
    BackgroundCall {
        pid,
        pgid: pid,
        birth: Some(u64::from(pid) * 1_000),
        exe: "python3".into(),
        task: None,
        quiet_ms: min * 60_000,
        wait: None,
    }
}

/// A reading taken at `as_of_ms` naming `calls`.
fn reading(as_of_ms: i64, calls: &[BackgroundCall]) -> FactsReading {
    FactsReading {
        as_of_ms,
        no_progress: calls.to_vec(),
        held: None,
        left: None,
    }
}

/// The episode of `c`.
fn key(c: &BackgroundCall) -> EpisodeKey {
    EpisodeKey::of_call(c).expect("a call with a birth")
}

/// The fact `pick` gives, or a panic naming why it gave none.
fn fact(p: Pick) -> Fact {
    match p {
        Pick::Fact(f) => f,
        Pick::Why(why) => panic!("no fact: {why}"),
    }
}

/// `r`'s pick for `reader` of `CLAIM` at `now_ms`, offered to it where it is a fact.
fn take(
    acc: &mut SelfClaimFacts<&'static str>,
    r: &FactsReading,
    reader: &str,
    now_ms: i64,
) -> Pick {
    acc.observe(Some(r), now_ms);
    let pick = acc.pick(Some(r), &CLAIM, reader, now_ms);
    if let Pick::Fact(f) = &pick {
        acc.offer(f, CLAIM, reader, now_ms);
    }
    pick
}

/// A STALLED CALL IS OFFERED ONCE AND TOLD ONCE: the planted call's fact (its sentence, its
/// episode, its reading's time), told by its ack, which gives the fact back, and never offered
/// again while the call stays stalled, even past the lease. Kills M1 (an ack that drops the offer
/// without `FactsState::told`).
#[test]
fn a_stalled_call_is_offered_once_and_told_once() {
    let a = call(51_240, 6);
    let mut acc = SelfClaimFacts::default();
    let first = reading(T0, std::slice::from_ref(&a));
    let offered = fact(take(&mut acc, &first, R1, T0));
    assert_eq!(
        offered,
        Fact {
            key: key(&a),
            text: a.told(),
            as_of_ms: T0,
        }
    );
    assert_eq!(
        offered.text,
        "a background call (pid 51240) has made no progress for 6 min"
    );
    assert_eq!(acc.ack(key(&a).id, &CLAIM, T0 + 1_000), Some(offered));
    assert_eq!(take(&mut acc, &first, R1, T0 + 1_000), Pick::Why(WHY_TOLD));
    let later = T0 + OFFER_MS + 1_000;
    assert_eq!(
        take(&mut acc, &reading(later, &[a]), R2, later),
        Pick::Why(WHY_TOLD),
        "told stays told past the lease"
    );
}

/// AN OFFER IS LEASED TO ITS READER AND TAKEN AGAIN BY IT: R1 takes A and takes it again within
/// the lease; R2 gets B beside it, or `offered` with A alone; past the lease R2 gets A. Kills M5
/// (no lease), M6 (a lease that never lapses) and M39 (a lease that blocks its own reader).
#[test]
fn an_offer_is_leased_to_its_reader_and_taken_again_by_it() {
    let (a, b) = (call(700, 9), call(800, 4));
    let mut acc = SelfClaimFacts::default();
    let alone = reading(T0, std::slice::from_ref(&a));
    assert_eq!(fact(take(&mut acc, &alone, R1, T0)).key, key(&a));
    let again = T0 + 2_000;
    let alone = reading(again, std::slice::from_ref(&a));
    assert_eq!(
        fact(take(&mut acc, &alone, R1, again)).key,
        key(&a),
        "an act decided again takes its own offer again"
    );
    assert_eq!(
        acc.pick(Some(&alone), &CLAIM, R2, again),
        Pick::Why(WHY_OFFERED),
        "another reader waits out the lease"
    );
    let both = reading(again, &[a.clone(), b.clone()]);
    assert_eq!(
        acc.pick(Some(&both), &CLAIM, R2, again),
        Pick::Fact(Fact {
            key: key(&b),
            text: b.told(),
            as_of_ms: again,
        }),
        "another reader takes the next untold call"
    );
    let lapsed = again + OFFER_MS;
    let alone = reading(lapsed, std::slice::from_ref(&a));
    assert_eq!(
        fact(take(&mut acc, &alone, R2, lapsed)).key,
        key(&a),
        "a lapsed lease frees the call for another reader"
    );
    assert_eq!(
        acc.pick(Some(&alone), &CLAIM, R1, lapsed),
        Pick::Why(WHY_OFFERED),
        "the new lease is the new reader's"
    );
}

/// TWO READERS, TWO EPISODES, BOTH TOLD: R1 is offered A and R2 B, each ack tells its own, and
/// neither is offered again to anyone. Kills M31 (one offer slot for the whole account, the first
/// draft's: B's offer replaces A's, and A's ack is refused).
#[test]
fn two_readers_two_episodes_are_both_told() {
    let (a, b) = (call(700, 9), call(800, 4));
    let mut acc = SelfClaimFacts::default();
    let both = reading(T0, &[a.clone(), b.clone()]);
    assert_eq!(fact(take(&mut acc, &both, R1, T0)).key, key(&a));
    assert_eq!(fact(take(&mut acc, &both, R2, T0)).key, key(&b));
    assert_eq!(
        acc.ack(key(&a).id, &CLAIM, T0 + 1).map(|f| f.key),
        Some(key(&a))
    );
    assert_eq!(
        acc.ack(key(&b).id, &CLAIM, T0 + 2).map(|f| f.key),
        Some(key(&b))
    );
    for reader in [R1, R2] {
        assert_eq!(
            take(&mut acc, &both, reader, T0 + 3),
            Pick::Why(WHY_TOLD),
            "{reader}"
        );
    }
}

/// A LATE ACK STILL TELLS: an offer past its lease that no other reader took is acknowledged by
/// its act's late ack; and once another reader took the lapsed episode, the first act's late ack
/// tells it (the episode and the claim's proof, never the reader, key an ack: its act started),
/// and the second's is stale. Kills M42 (an ack honoured only within its lease, or only from the
/// lease's current reader).
#[test]
fn a_late_ack_after_a_lapse_still_tells() {
    let a = call(700, 9);
    let mut acc = SelfClaimFacts::default();
    assert_eq!(
        fact(take(
            &mut acc,
            &reading(T0, std::slice::from_ref(&a)),
            R1,
            T0
        ))
        .key,
        key(&a)
    );
    let late = T0 + 2 * OFFER_MS;
    assert_eq!(
        acc.ack(key(&a).id, &CLAIM, late).map(|f| f.key),
        Some(key(&a)),
        "a lapsed offer's late ack"
    );

    let mut acc = SelfClaimFacts::default();
    assert_eq!(
        fact(take(
            &mut acc,
            &reading(T0, std::slice::from_ref(&a)),
            R1,
            T0
        ))
        .key,
        key(&a)
    );
    let lapsed = T0 + OFFER_MS;
    assert_eq!(
        fact(take(
            &mut acc,
            &reading(lapsed, std::slice::from_ref(&a)),
            R2,
            lapsed
        ))
        .key,
        key(&a)
    );
    assert_eq!(
        acc.ack(key(&a).id, &CLAIM, lapsed + 1).map(|f| f.key),
        Some(key(&a)),
        "R1's late ack tells, though R2 holds the lease"
    );
    assert_eq!(acc.ack(key(&a).id, &CLAIM, lapsed + 2), None, "R2's ack");
}

/// A FOREIGN OR UNOFFERED ACK CHANGES NOTHING: an ack under another claim's proof, and an ack of
/// an episode never offered, give `None`, and the account offers exactly what it offered before.
/// Kills M3 (told before the claim check) and M4 (any episode acknowledged).
#[test]
fn a_foreign_or_unoffered_ack_changes_nothing() {
    let (a, b) = (call(700, 9), call(800, 4));
    let mut acc = SelfClaimFacts::default();
    let both = reading(T0, &[a.clone(), b.clone()]);
    assert_eq!(fact(take(&mut acc, &both, R1, T0)).key, key(&a));
    let before = |acc: &SelfClaimFacts<&'static str>| {
        [R1, R2].map(|reader| acc.pick(Some(&both), &CLAIM, reader, T0 + 1))
    };
    let was = before(&acc);
    assert_eq!(acc.ack(key(&a).id, &OTHER, T0 + 1), None, "another proof");
    assert_eq!(acc.ack(key(&b).id, &CLAIM, T0 + 1), None, "never offered");
    assert_eq!(acc.ack(0x51, &CLAIM, T0 + 1), None, "no such episode");
    assert_eq!(before(&acc), was);
    assert_eq!(
        acc.ack(key(&a).id, &CLAIM, T0 + 2).map(|f| f.key),
        Some(key(&a)),
        "its own claim's ack still tells it"
    );
}

/// A READING TOO OLD TO TELL OFFERS NOTHING: an untold call in a view 6 s old is `stale`; the
/// same call 4 s old is offered. Kills M9 (`fresh_at`, the 60 s bound that may forget, in place
/// of `tells_at`, the 5 s one a fact is told from).
#[test]
fn a_reading_too_old_to_tell_offers_nothing() {
    let a = call(700, 9);
    let acc: SelfClaimFacts<&'static str> = SelfClaimFacts::default();
    let now = T0 + 6_000;
    assert!(now - T0 > FACT_FRESH_MS);
    let old = reading(T0, std::slice::from_ref(&a));
    assert_eq!(acc.pick(Some(&old), &CLAIM, R1, now), Pick::Why(WHY_STALE));
    assert_eq!(acc.pick(None, &CLAIM, R1, now), Pick::Why(WHY_STALE));
    let young = reading(now - 4_000, std::slice::from_ref(&a));
    assert_eq!(fact(acc.pick(Some(&young), &CLAIM, R1, now)).key, key(&a));
    assert_eq!(
        acc.pick(Some(&reading(now, &[])), &CLAIM, R1, now),
        Pick::Why(WHY_NONE),
        "a fresh reading that names no call"
    );
}

/// A CLOSED RUN IS FORGOTTEN AND THE NEXT ONE OFFERED: a told call that a fresh reading no
/// longer names (it made progress) is offered again once it stalls again; and an offer whose
/// call a fresh reading no longer names is dropped, so its next run is any reader's at once.
/// Kills M11 (no observe).
#[test]
fn a_closed_run_is_forgotten_and_the_next_one_offered() {
    let a = call(700, 9);
    let mut acc = SelfClaimFacts::default();
    let stalled = |at| reading(at, std::slice::from_ref(&a));
    assert_eq!(fact(take(&mut acc, &stalled(T0), R1, T0)).key, key(&a));
    assert!(acc.ack(key(&a).id, &CLAIM, T0 + 1).is_some());
    assert_eq!(
        take(&mut acc, &stalled(T0 + 2), R1, T0 + 2),
        Pick::Why(WHY_TOLD)
    );
    assert_eq!(
        take(&mut acc, &reading(T0 + 3, &[]), R1, T0 + 3),
        Pick::Why(WHY_NONE)
    );
    assert_eq!(
        fact(take(&mut acc, &stalled(T0 + 4), R1, T0 + 4)).key,
        key(&a),
        "the next run of no progress is told again"
    );
    assert_eq!(
        take(&mut acc, &reading(T0 + 5, &[]), R2, T0 + 5),
        Pick::Why(WHY_NONE)
    );
    assert_eq!(
        fact(take(&mut acc, &stalled(T0 + 6), R2, T0 + 6)).key,
        key(&a),
        "the closed run's offer to R1 was dropped"
    );
}

/// THE ACCOUNT IS SEEDED ONCE: the ledger is read into it once per process, and a later seed
/// changes nothing.
#[test]
fn the_account_is_seeded_once() {
    let a = call(700, 9);
    let mut acc: SelfClaimFacts<&'static str> = SelfClaimFacts::default();
    assert!(!acc.seeded());
    acc.seed(&[], T0);
    assert!(acc.seeded());
    acc.seed(&[(key(&a), T0, SeedOutcome::Typed)], T0);
    let stalled = reading(T0, std::slice::from_ref(&a));
    assert_eq!(fact(acc.pick(Some(&stalled), &CLAIM, R1, T0)).key, key(&a));
}

/// A SUCCESSOR SEEDS ITS ACCOUNT FROM THE LEDGER: the told row a self-claim's ack books
/// ([`super::super::approvals::SelfClaimFactRow`]), read back by
/// [`super::super::occupancy::door::seed_rows_in`] into a new process's account while the call
/// is still open, leaves it told there, so a restart of aterm never tells an episode twice.
/// CONTROL: a new account with no seed offers it.
#[test]
fn a_successor_seeds_its_account_from_the_ledger() {
    use super::super::approvals::SelfClaimFactRow;
    use super::super::occupancy::door::seed_rows_in;
    const SID: &str = "@s-1";
    let a = call(700, 9);
    let stalled = |at| reading(at, std::slice::from_ref(&a));
    let mut first: SelfClaimFacts<&'static str> = SelfClaimFacts::default();
    first.seed(&[], T0);
    let offered = fact(take(&mut first, &stalled(T0), R1, T0));
    let told = first.ack(key(&a).id, &CLAIM, T0 + 1).expect("told");
    let body = format!(
        "{}\n",
        SelfClaimFactRow {
            holder: "codex-alab@4242",
            fact: &told,
        }
        .to_json(T0 + 1, Some(SID))
    );
    let later = T0 + 2 * FACT_FRESH_MS;
    let mut next: SelfClaimFacts<&'static str> = SelfClaimFacts::default();
    next.seed(&seed_rows_in(&body, Some(SID)), later);
    assert_eq!(
        take(&mut next, &stalled(later), R1, later),
        Pick::Why(WHY_TOLD),
        "{body}"
    );
    let mut unseeded: SelfClaimFacts<&'static str> = SelfClaimFacts::default();
    assert_eq!(
        fact(take(&mut unseeded, &stalled(later), R1, later)).key,
        offered.key,
        "CONTROL: unseeded, the same episode is offered"
    );
}
