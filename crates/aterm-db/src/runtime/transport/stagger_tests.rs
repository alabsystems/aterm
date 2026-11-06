// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CR2: dial admission under an injected clock. A slow endpoint holds its own dial, never the
//! next one for longer than a stagger; dials stay within their bound; and once a peer is found,
//! a serving node runs one discovery dial at a time beside its peers' rounds ([`dial_room`]).

use super::*;
use std::collections::BTreeMap;

/// What a simulated endpoint does with a dial ([`run`]).
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// Its peer authenticates at once, and the round completes this long after the dial.
    After(Duration),
    /// Its accept queue never reaches the dial, or nothing answers the SYN: the dial ends at the
    /// handshake limit with no peer authenticated, and nothing said no peer is there.
    Never,
}

/// The longest a dial nobody answers is held: the TLS handshake limit
/// (`crate::control::Endpoint::exchange`), past the 3 s connect limit.
const HANDSHAKE: Duration = Duration::from_secs(30);

/// What a simulated discovery run did, each time from its start.
#[derive(Debug, Default)]
struct Run {
    /// When the first round with an endpoint that answers completed.
    contact: Option<Duration>,
    /// When each dial started, and where to.
    starts: Vec<(Duration, SocketAddr)>,
    /// The most dials open at once.
    most_open: usize,
    /// The most dials open at once from [`HANDSHAKE`] after the first contact on, when every dial
    /// the search left open has ended.
    most_open_settled: usize,
}

/// Drive `hints` as the transport's tick does, every runtime tick for `horizon`, with every
/// endpoint in `answers` beaconing every [`BEACON`] and behaving as its answer says: a dial starts
/// only where [`next_dial`] admits one, and each ends as its endpoint answers ([`Hints::finish`]).
fn run(
    hints: &mut Hints,
    answers: &BTreeMap<SocketAddr, Answer>,
    pending: bool,
    horizon: Duration,
) -> Run {
    let start = Instant::now();
    let mut dial = start;
    let mut beacon = start;
    let mut open: Vec<(SocketAddr, Instant)> = Vec::new();
    let mut run = Run::default();
    let mut now = start;
    while now <= start + horizon {
        if now >= beacon {
            for (n, address) in answers.keys().enumerate() {
                let wire = Hints::new([n as u8 + 10; 32]).wire(address.port());
                hints.observe(&wire, *address, now);
            }
            beacon = now + BEACON;
        }
        let ends = |(address, started): &(SocketAddr, Instant)| match answers[address] {
            Answer::After(within) => *started + within,
            Answer::Never => *started + HANDSHAKE,
        };
        let (ended, still): (Vec<_>, Vec<_>) = open.into_iter().partition(|dial| ends(dial) <= now);
        open = still;
        for (address, started) in ended {
            let outcome = match answers[&address] {
                Answer::After(within) => {
                    run.contact.get_or_insert(started + within - start);
                    // Each run has one peer that answers.
                    (
                        Repair::heard_from([1; 48]),
                        Outcome::Repaired { transferred: false },
                    )
                }
                Answer::Never => (Repair::default(), Outcome::Refused),
            };
            let (repair, outcome) = outcome;
            hints.finish(address, repair, outcome, now);
        }
        let dialing: Vec<_> = open
            .iter()
            .map(|(address, _)| (address, answers[address]))
            .zip(&open)
            .map(|((address, answer), (_, started))| Dialing {
                started: *started,
                established: hints.established(*address),
                heard: matches!(answer, Answer::After(_)),
            })
            .collect();
        if let Some((address, _)) = next_dial(hints, &dialing, pending, &mut dial, now) {
            open.push((address, now));
            run.starts.push((now - start, address));
        }
        run.most_open = run.most_open.max(open.len());
        if run
            .contact
            .is_some_and(|contact| now - start >= contact + HANDSHAKE)
        {
            run.most_open_settled = run.most_open_settled.max(open.len());
        }
        now += super::super::TICK;
    }
    run
}

/// Endpoints that never answer at `10.1.1.1` up, `slow` of them, and one peer that answers in
/// `round` at `10.1.9.9`, after them in address order.
fn behind(slow: u8, round: Duration) -> BTreeMap<SocketAddr, Answer> {
    let mut answers: BTreeMap<_, _> = (1..=slow)
        .map(|n| (SocketAddr::from(([10, 1, 1, n], 4000)), Answer::Never))
        .collect();
    answers.insert(
        SocketAddr::from(([10, 1, 9, 9], 4000)),
        Answer::After(round),
    );
    answers
}

/// CR2's second law: slow endpoints ahead of a peer each delay its first contact by at most one
/// stagger, whether they hold both places a serving node's rounds have (the two slow endpoints
/// E1b measured, .172 and .203) or more, up to the bound.
///
/// NEGATIVE CONTROL (run here, [`UNSTAGGERED`]): when every dial holds its place until it ends,
/// as before CR2, two slow endpoints hold the peer's dial for the whole handshake limit.
#[test]
fn slow_endpoints_delay_first_contact_by_at_most_one_stagger_each() {
    for slow in [ROUNDS as u8, DIALS as u8 - 1] {
        for unstaggered in [false, true] {
            let answers = behind(slow, Duration::from_millis(50));
            let mut hints = Hints::new([1; 32]);
            UNSTAGGERED.with(|cell| cell.set(unstaggered));
            let contact = run(&mut hints, &answers, false, 4 * HANDSHAKE).contact;
            UNSTAGGERED.with(|cell| cell.set(false));
            let contact = contact.expect("the peer is reached within the run");
            if unstaggered {
                assert!(
                    contact >= HANDSHAKE,
                    "MUTANT: {slow} slow endpoints hold the dial until their limit: {contact:?}"
                );
                continue;
            }
            assert!(
                contact <= STAGGER * u32::from(slow) + Duration::from_millis(50),
                "{slow} slow endpoints delayed first contact to {contact:?}"
            );
        }
    }
}

/// CR2's third law: however many endpoints never answer, at most [`DIALS`] dials are open at
/// once, a serving node's or an enrolling one's, and two dials start at least [`SPACING`] apart.
/// Seventy Macs that start together on such a LAN hold at most 70 × [`DIALS`] connections.
///
/// NEGATIVE CONTROL (run here, [`UNBOUNDED`]): with parked dials uncounted, a new dial opens every
/// spacing until the first reaches its limit.
#[test]
fn dials_stay_within_their_bound_when_nothing_answers() {
    for pending in [false, true] {
        for unbounded in [false, true] {
            let answers: BTreeMap<_, _> = (1..=50u8)
                .map(|n| (SocketAddr::from(([10, 1, 1, n], 4000)), Answer::Never))
                .collect();
            let mut hints = Hints::new([1; 32]);
            UNBOUNDED.with(|cell| cell.set(unbounded));
            let run = run(&mut hints, &answers, pending, Duration::from_secs(180));
            UNBOUNDED.with(|cell| cell.set(false));
            if unbounded {
                assert!(run.most_open > DIALS, "MUTANT: {} open", run.most_open);
                continue;
            }
            assert_eq!(run.most_open, DIALS, "pending={pending}: {run:?}");
            assert!(
                run.starts
                    .windows(2)
                    .all(|pair| pair[1].0 - pair[0].0 >= SPACING),
                "dials closer than the spacing: {:?}",
                run.starts
            );
        }
    }
}

/// Once a serving node has found a peer, a crowd that never answers costs it one discovery dial
/// at a time, as before CR2: it holds at most `ROUNDS + 1` dials once those its search left have
/// ended, and its peer's rounds go on at their own pace beside them.
///
/// NEGATIVE CONTROL (run here, [`PARKS_ALWAYS`]): discovery dials that park whether or not a
/// peer is found fill all [`DIALS`] in steady state, on every Mac of a crowded LAN.
#[test]
fn a_node_that_found_its_peer_runs_one_discovery_dial_at_a_time() {
    for parks_always in [false, true] {
        // The peer first in address order, so it is found at once and the rest is steady state.
        let peer = SocketAddr::from(([10, 1, 0, 9], 4000));
        let mut answers = behind(30, Duration::from_secs(2));
        answers.remove(&SocketAddr::from(([10, 1, 9, 9], 4000)));
        answers.insert(peer, Answer::After(Duration::from_secs(2)));
        let mut hints = Hints::new([1; 32]);
        PARKS_ALWAYS.with(|cell| cell.set(parks_always));
        let run = run(&mut hints, &answers, false, Duration::from_secs(300));
        PARKS_ALWAYS.with(|cell| cell.set(false));
        let contact = run.contact.expect("the peer is reached");
        if parks_always {
            assert_eq!(
                run.most_open_settled, DIALS,
                "MUTANT: the crowd fills every dial"
            );
            continue;
        }
        assert!(
            run.most_open_settled <= ROUNDS + 1,
            "{} dials open after the search: {run:?}",
            run.most_open_settled
        );
        let rounds = run
            .starts
            .iter()
            .filter(|(at, address)| *address == peer && *at > contact)
            .count();
        assert!(rounds >= 20, "the peer's rounds went on: {rounds} in 300 s");
    }
}

/// Where a dial holds a place: an established peer's round, or a dial whose peer authenticated,
/// always; a discovery dial for its stagger. A serving node that has found a peer keeps a place
/// for its peers' rounds and runs one discovery dial at a time; an enrolling node, with no
/// listener, runs [`INCOMING`] more rounds and keeps none.
#[test]
fn dial_room_places_rounds_and_staggering_dials() {
    let now = Instant::now() + Duration::from_secs(60);
    let dial = |age: Duration, established: bool, heard: bool| Dialing {
        started: now - age,
        established,
        heard,
    };
    let young = dial(STAGGER / 2, false, false);
    let parked = dial(STAGGER, false, false);
    let heard = dial(STAGGER * 4, false, true);
    let round = dial(HANDSHAKE, true, false);
    // A serving node before any peer: two staggering dials fill its places; a parked one holds
    // none, and discovery goes on beside it.
    assert_eq!(dial_room(&[young, young], false, false, now), None);
    assert_eq!(dial_room(&[young, parked], false, false, now), Some(true));
    assert_eq!(
        dial_room(&[parked; DIALS - 1], false, false, now),
        Some(true)
    );
    // A dial whose peer authenticated holds its place however long it runs, and has found a peer.
    assert_eq!(dial_room(&[heard, young], false, false, now), None);
    assert_eq!(dial_room(&[heard], false, false, now), Some(true));
    assert_eq!(dial_room(&[heard, parked], false, false, now), Some(false));
    // Once it has an established peer, one discovery dial at a time, parked or not, beside its
    // peers' rounds, which the dials its search left parked never keep out.
    assert_eq!(dial_room(&[], false, true, now), Some(true));
    assert_eq!(dial_room(&[young], false, true, now), Some(false));
    assert_eq!(dial_room(&[parked], false, true, now), Some(false));
    assert_eq!(
        dial_room(&[parked; DIALS - 1], false, true, now),
        Some(false)
    );
    assert_eq!(dial_room(&[round, round], false, true, now), None);
    assert_eq!(dial_room(&[round, young], false, true, now), None);
    // An enrolling node runs four rounds and reserves none.
    assert_eq!(dial_room(&[young; 3], true, false, now), Some(true));
    assert_eq!(dial_room(&[young; 4], true, false, now), None);
    assert_eq!(dial_room(&[young, parked], true, true, now), Some(true));
    // Parked dials count against the bound.
    assert_eq!(dial_room(&[parked; DIALS], false, false, now), None);
    assert_eq!(dial_room(&[parked; DIALS], true, false, now), None);
}
