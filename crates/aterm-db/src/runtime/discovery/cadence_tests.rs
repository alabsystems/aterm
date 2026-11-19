// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The dial scheduler's conformance to `DbDialCadence` (`aterm_models_db::dial_cadence_model`):
//! each abstract state is built as one endpoint's [`Peer`] (and, for the walk, three resting
//! seats with a moved table), the action runs on the code, and what the code left is read back
//! into the model's terms.

use super::*;
use aterm_spec::interp::State;

/// The model's unit of time: two minutes.
const UNIT: Duration = Duration::from_secs(120);
/// The endpoint's seat.
const ACTOR: Actor = [2; 48];
/// The table the walk offers.
const MOVED: Digest = [7; 32];
/// Another table: what a round over the endpoint takes, and what a seat that lacks the moved
/// one takes instead.
const OTHER: Digest = [8; 32];

/// How long a silence lasts that calls for the probe whatever this node's jitter for it.
fn silence() -> Duration {
    SILENCE + Duration::from_millis(SILENCE_JITTER_MS) + Duration::from_secs(1)
}

/// A wait's category ([`aterm_models_db::dial_cadence_model`]'s `wait`): 0 within the ordinary
/// interval (a retry, a follow-up), 1 for a failed dial's backoff, else its whole units.
fn category(wait: Duration) -> i64 {
    if wait < Duration::from_secs(5) {
        0
    } else if wait <= Duration::from_secs(122) {
        1
    } else {
        i64::try_from(wait.as_secs() / UNIT.as_secs()).expect("small")
    }
}

/// A count of units in `state`.
fn units(state: &State, var: &str) -> u32 {
    u32::try_from(state[var]).expect("a small count")
}

/// One endpoint, `10.1.1.2:1234`, in the abstract `state` at `now`. One its seat was heard by,
/// in a round dialled in from its address since its last silence (`vouched`), last beaconed as
/// long ago as a silence: only that round keeps it from going silent.
fn endpoint(state: &State, now: Instant) -> (Hints, SocketAddr) {
    let address: SocketAddr = "10.1.1.2:1234".parse().unwrap();
    let mut hints = Hints::new([4; 32]);
    let silent = state["silent"] == 1;
    let vouched = state["vouched"] == 1;
    let busy = state["busy"] == 1;
    let rest = state["rest"] == 1;
    let failing = state["failing"] == 1;
    let seen = now - UNIT * units(state, "age") - if silent { silence() } else { Duration::ZERO };
    let next = now
        + match state["wait"] {
            0 => Duration::ZERO,
            1 => Duration::from_secs(60),
            _ => UNIT * units(state, "wait"),
        };
    let peer = Peer {
        seen,
        next,
        verified: true,
        busy,
        failed: failing,
        failures: u32::from(failing),
        pulled: state["pulled"] == 1,
        dialled: busy.then_some(ACTOR),
        began: if busy { now } else { seen },
        beacon: (state["beaconed"] == 1).then(|| {
            if silent || vouched {
                now - silence()
            } else {
                now
            }
        }),
        dialled_in: vouched.then(|| if silent { now - silence() } else { now }),
        probed: state["probed"] == 1,
        standing: Standing::There,
        // At rest its next dial is its audit ([`Peer::resting`]); else its audit is further off
        // than any wait the model names.
        audit_due: Some(if rest { next } else { now + VERIFIED_EXPIRES }),
        repair: if busy {
            Repair::default()
        } else {
            Repair::heard_from(ACTOR)
        },
        ..Peer::candidate(seen, Lead::None)
    };
    hints.peers.insert(address, peer);
    (hints, address)
}

/// Whether `peer` has gone silent at `now`: it beaconed, and neither a beacon, a completed round
/// nor a round its seat dialled in from its address was heard for longer than any probe's
/// jitter.
fn silent(peer: &Peer, now: Instant) -> bool {
    peer.beacon.is_some_and(|beacon| {
        heard_since(
            beacon
                .max(peer.seen)
                .max(peer.dialled_in.unwrap_or(peer.seen)),
            now,
        )
    })
}

/// Whether what was heard `at` is no longer heard at `now`: longer ago than any probe's jitter.
fn heard_since(at: Instant, now: Instant) -> bool {
    at + SILENCE + Duration::from_millis(SILENCE_JITTER_MS) <= now
}

/// Read the endpoint at `address` back into `next`.
fn read(next: &mut State, hints: &Hints, address: SocketAddr, now: Instant) {
    let peer = &hints.peers[&address];
    next.insert("busy", i64::from(peer.busy));
    next.insert("wait", category(peer.next.saturating_duration_since(now)));
    next.insert(
        "age",
        i64::try_from(now.saturating_duration_since(peer.began).as_secs() / UNIT.as_secs())
            .expect("small"),
    );
    next.insert("failing", i64::from(peer.failed || peer.failures > 0));
    next.insert("rest", i64::from(peer.resting()));
    next.insert("pulled", i64::from(peer.pulled));
    next.insert("beaconed", i64::from(peer.beacon.is_some()));
    next.insert("silent", i64::from(silent(peer, now)));
    next.insert("probed", i64::from(peer.probed));
    next.insert(
        "vouched",
        i64::from(peer.dialled_in.is_some_and(|at| !heard_since(at, now))),
    );
}

/// What a beacon, a round or a vouch may not move unless it is a grant: the endpoint's wait,
/// verification, contact and failure count.
fn grants(peer: &Peer) -> String {
    format!(
        "{:?}",
        (
            peer.next,
            peer.verified,
            peer.seen,
            peer.began,
            peer.resting(),
            peer.audit_due,
            peer.failed,
            peer.failures,
            peer.pulled,
        )
    )
}

/// The wait before a beacon or a vouch as the model reads it (`before`): the endpoint's wait in
/// `state`, or -1, a wait no state has, where the code's step moved a grant ([`grants`]), even
/// one the wait's categories do not show; such a step lands on no successor and breaks the law.
fn before(state: &State, moved: bool) -> i64 {
    if moved { -1 } else { state["wait"] }
}

/// The round the peer's answer in `state` makes: a failure, one that broke off in its
/// application exchange after authenticating, or a completed round that moved records or not,
/// whose scan has or has not gone round and audited, and whose hint does or does not still name
/// a table.
fn round(state: &State) -> (Repair, Outcome) {
    let mut repair = Repair::heard_from(ACTOR);
    if state["fails"] == 1 {
        repair.disown();
        return (repair, Outcome::Refused);
    }
    if state["breaks"] == 1 {
        repair.application_failed(crate::Error::repair(
            "the table exchange broke off",
            aterm_types::network_state::RepairCause::Io,
        ));
        if state["owes"] == 1 {
            assert!(repair.lead(MOVED));
        }
        let outcome = Outcome::of(true, &repair, None);
        return (repair, outcome);
    }
    let report = crate::replica::Report {
        table: Some(OTHER),
        caught_up: true,
        asked: Some(crate::replica::Ask::First),
        turn: Some(crate::replica::TableTurn::Regular),
        ..Default::default()
    };
    let mut repair = if state["sweeps"] == 1 {
        repair.applied(crate::replica::Report::default())
    } else {
        repair.swept(4).applied(report)
    };
    if state["owes"] == 1 {
        assert!(repair.lead(MOVED));
    }
    let transferred = state["moves"] == 1;
    (repair, Outcome::Repaired { transferred })
}

/// Three seats at `10.1.1.1..=3`, each resting after a quiet round at `start`, and the moved
/// table the abstract `state`'s walk offers: tried by its first `tried` seats, with `cold` cold
/// answers in a row.
fn walk(state: &State, start: Instant) -> (Hints, Vec<(SocketAddr, Actor)>) {
    let mut hints = Hints::new([4; 32]);
    let seats: Vec<(SocketAddr, Actor)> = (1..=3)
        .map(|n| {
            let address = SocketAddr::from(([10, 1, 1, n], 1234));
            hints.observe(&Hints::new([12; 32]).wire(1234), address, start);
            let (chosen, _) = hints.next(start).unwrap();
            assert_eq!(chosen, address);
            let quiet = Outcome::Repaired { transferred: false };
            hints.finish(address, Repair::heard_from([n; 48]), quiet, start);
            hints.peers.get_mut(&address).unwrap().beacon = None;
            (address, [n; 48])
        })
        .collect();
    if state["hot"] == 1 {
        let tried = usize::try_from(state["tried"]).expect("a seat count");
        hints.hot.insert(
            Subject::Table(MOVED),
            Hot {
                since: start,
                held: None,
                cold: usize::try_from(state["cold"]).expect("a count"),
                tried: seats[..tried].iter().map(|(_, actor)| *actor).collect(),
            },
        );
    }
    (hints, seats)
}

/// A completed round with the seat `actor` in which this side listed short
/// (`crate::replica::Report::listed_short`): its selection fell back to the recovery region.
fn short(actor: Actor) -> Repair {
    Repair::heard_from(actor).applied(crate::replica::Report {
        primary_unread: true,
        ..Default::default()
    })
}

/// Read the walk back into `next`.
fn read_walk(next: &mut State, hints: &Hints) {
    let hot = hints.hot.get(&Subject::Table(MOVED));
    next.insert("hot", i64::from(hot.is_some()));
    next.insert(
        "tried",
        hot.map_or(0, |hot| i64::try_from(hot.tried.len()).expect("small")),
    );
    next.insert(
        "cold",
        hot.map_or(0, |hot| i64::try_from(hot.cold).expect("small")),
    );
}

/// The code's step for `action` from the abstract `state`, read back into the model's terms.
pub(crate) fn dial_cadence_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    let now = Instant::now() + Duration::from_secs(3600);
    match action {
        // The clock and the peer's answer: model bookkeeping (`db_dial_cadence_environment`).
        "Tick" => {
            next.insert("wait", state["wait"] - 1);
            next.insert("age", state["age"] + 1);
            next.insert("last", 0);
            next.insert("before", 0);
        }
        "Silence" => {
            next.insert("silent", 1);
            next.insert("vouched", 0);
        }
        "Owes" => {
            next.insert("owes", 1);
        }
        "Sweeps" => {
            next.insert("sweeps", 1);
        }
        "Moves" => {
            next.insert("moves", 1);
        }
        "Fails" => {
            next.insert("fails", 1);
        }
        "Breaks" => {
            next.insert("breaks", 1);
        }
        "Beacon" => {
            let (mut hints, address) = endpoint(state, now);
            let before = grants(&hints.peers[&address]);
            hints.observe(
                &Hints::new([12; 32]).wire(address.port()),
                SocketAddr::new(address.ip(), 9),
                now,
            );
            let granted = grants(&hints.peers[&address]) != before;
            read(&mut next, &hints, address, now);
            next.insert("probes", 0);
            next.insert("last", 1);
            next.insert("before", self::before(state, granted));
        }
        "Dial" => {
            let (mut hints, address) = endpoint(state, now);
            let probing = hints
                .probe_at(&address, &hints.peers[&address])
                .is_some_and(|probe| probe <= now);
            let Some((chosen, _)) = hints.next(now) else {
                return next;
            };
            assert_eq!(chosen, address);
            read(&mut next, &hints, address, now);
            for var in [
                "owes", "sweeps", "moves", "fails", "breaks", "last", "before",
            ] {
                next.insert(var, 0);
            }
            if probing {
                next.insert("probes", (state["probes"] + 1).min(2));
                if state["vouched"] == 1 {
                    next.insert("misprobed", 1);
                }
            }
        }
        "Finish" => {
            let (mut hints, address) = endpoint(state, now);
            let (repair, outcome) = round(state);
            hints.finish(address, repair, outcome, now);
            read(&mut next, &hints, address, now);
            for var in ["moves", "fails", "last", "before"] {
                next.insert(var, 0);
            }
        }
        "Vouch" => {
            let (mut hints, address) = endpoint(state, now);
            let healthy = state["failing"] == 0;
            let before = grants(&hints.peers[&address]);
            hints.vouch(address.ip(), ACTOR, now);
            let stirred = healthy && grants(&hints.peers[&address]) != before;
            read(&mut next, &hints, address, now);
            next.insert("last", 2);
            next.insert("before", self::before(state, stirred));
        }
        "Move" => {
            let (mut hints, _) = walk(state, now - Duration::from_secs(1));
            hints.moved(Subject::Table(MOVED), None, [], now);
            read_walk(&mut next, &hints);
            next.insert("walked", 0);
        }
        "WalkSends" | "WalkCold" | "WalkLacks" => {
            let (mut hints, seats) = walk(state, now - Duration::from_secs(1));
            let Some((address, repair)) = hints.next(now) else {
                return next;
            };
            assert!(
                repair.leads(&MOVED),
                "a walk's dial is handed the moved table"
            );
            let actor = seats.iter().find(|(at, _)| *at == address).unwrap().1;
            let report = crate::replica::Report {
                table: Some(if action == "WalkLacks" { OTHER } else { MOVED }),
                sent: usize::from(action == "WalkSends"),
                caught_up: true,
                ..Default::default()
            };
            let repair = Repair::heard_from(actor).applied(report);
            hints.settle(actor, Some(address), now, &repair, now);
            let outcome = Outcome::of(true, &repair, None);
            hints.finish(address, repair, outcome, now);
            hints.cool();
            read_walk(&mut next, &hints);
            next.insert("walked", (state["walked"] + 1).min(4));
        }
        "Owe" => {
            let (mut hints, seats) = walk(state, now - Duration::from_secs(1));
            let (_, actor) = seats[0];
            hints.settle(actor, None, now, &short(actor), now);
            read_walk(&mut next, &hints);
            next.insert("owed", i64::from(hints.owes(actor)));
            next.insert("owedwalks", 0);
        }
        "WalkOwed" => {
            // The seats of a walk with nothing moved: only the owe asks for a dial.
            let mut quiet = state.clone();
            quiet.insert("hot", 0);
            let (mut hints, seats) = walk(&quiet, now - Duration::from_secs(1));
            let (address, actor) = seats[0];
            hints.settle(actor, None, now, &short(actor), now);
            let later = now + Duration::from_secs(10);
            let Some((chosen, repair)) = hints.next(later) else {
                return next;
            };
            assert_eq!(chosen, address, "the walk dials the seat it owes");
            assert!(!repair.audited(), "an owed round is no audit");
            let whole = Repair::heard_from(actor).applied(crate::replica::Report {
                table: Some(OTHER),
                caught_up: true,
                ..Default::default()
            });
            hints.settle(actor, Some(address), later, &whole, later);
            let outcome = Outcome::of(true, &whole, None);
            hints.finish(address, whole, outcome, later);
            next.insert("owed", i64::from(hints.owes(actor)));
            next.insert("owedwalks", (state["owedwalks"] + 1).min(2));
        }
        _ => panic!("unknown DbDialCadence action {action}"),
    }
    next
}

/// `body`'s result with the mutant `flag` on for this thread.
fn under<R>(
    flag: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
    body: impl FnOnce() -> R,
) -> R {
    flag.with(|flag| flag.set(true));
    let result = body();
    flag.with(|flag| flag.set(false));
    result
}

/// A code step that landed on none of the model's successors: the action, the state it ran
/// from, and the state the code left.
type Stray = (&'static str, State, State);

/// How often each action ran from a reachable state of `model`, and each code step that landed
/// on none of its successors.
fn steps(model: &aterm_spec::derive::Model) -> (BTreeMap<&'static str, usize>, Vec<Stray>) {
    let mut observed = BTreeMap::new();
    let mut strays = Vec::new();
    for state in aterm_spec::interp::reachable_states(model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = dial_cadence_transition(&state, action.name);
                if !model.successors(action.name, &state).contains(&next) {
                    strays.push((action.name, state.clone(), next));
                }
                *observed.entry(action.name).or_default() += 1;
            }
        }
    }
    (observed, strays)
}

/// The scheduler refines `DbDialCadence`: from every reachable state, each enabled action's code
/// step lands on one of the model's successors, and under each of nine code mutants some step
/// lands on none. A round its seat dialled in from its address ends a silence, so a dial from a
/// state that round was heard in probes nothing, and a probe already spent stays spent.
///
/// NEGATIVE CONTROLS (run here), each a code mutant whose step leaves the model or breaks its law:
/// `BEACON_DEFERS_AUDIT` (a beacon moves a due endpoint's wait: `BeaconGrantsNothing`),
/// `PROBE_REPEATS` (a probed silence is probed again: `SilenceProbesOnce`), `RESTS_PENDING` (a
/// round that owes rests: `PendingNeverRests`), `UNFINISHED_FINISHES` (a round that broke off
/// counts as a quiet one and rests: `PendingNeverRests`), `REPULLS` (a round dialled in pulls in
/// a resting endpoint: `HealthyVouchInert`), `QUIET_REDIALS` (a quiet round's endpoint does not
/// rest: no successor), `LED_UNSETTLED` (a seat that lacks the table is never settled:
/// `WalkEnds`), `INBOUND_UNHEARD` (a silence heard over by a round dialled in is probed:
/// `VouchEndsSilence`), `VOUCH_REARMS` (a round dialled in re-arms a spent probe: no
/// successor), `SHORT_UNOWED` (a short list owes nothing: no successor) and `OWED_KEPT` (the
/// owed round leaves the owe standing: `OwedWalksOnce`).
#[test]
fn the_dial_scheduler_conforms_to_its_cadence_model() {
    let model = aterm_models_db::dial_cadence_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let (observed, strays) = steps(&model);
    assert_eq!(strays, [], "code steps that leave the model");
    for action in &model.actions {
        assert!(
            observed.contains_key(action.name),
            "{} never ran",
            action.name
        );
    }
    for flag in [
        &RESTS_PENDING,
        &UNFINISHED_FINISHES,
        &QUIET_REDIALS,
        &REPULLS,
        &LED_UNSETTLED,
        &INBOUND_UNHEARD,
        &VOUCH_REARMS,
        &SHORT_UNOWED,
        &OWED_KEPT,
    ] {
        assert!(
            !under(flag, || steps(&model)).1.is_empty(),
            "MUTANT: every step conforms"
        );
    }

    let mut idle = model.init_state();
    idle.insert("beaconed", 1);
    let deferred = under(&BEACON_DEFERS_AUDIT, || {
        dial_cadence_transition(&idle, "Beacon")
    });
    assert!(!model.check_invariant("BeaconGrantsNothing", &deferred));

    let mut probed = model.init_state();
    for (var, value) in [("beaconed", 1), ("silent", 1), ("probed", 1), ("probes", 1)] {
        probed.insert(var, value);
    }
    probed.insert("wait", 3);
    probed.insert("rest", 1);
    assert_eq!(
        dial_cadence_transition(&probed, "Dial"),
        probed,
        "one probe a silence"
    );
    let again = under(&PROBE_REPEATS, || dial_cadence_transition(&probed, "Dial"));
    assert!(!model.check_invariant("SilenceProbesOnce", &again));

    let heard = dial_cadence_transition(&probed, "Vouch");
    assert!(
        model.successors("Vouch", &probed).contains(&heard),
        "{heard:?}"
    );
    assert_eq!(
        (heard["silent"], heard["probed"], heard["vouched"]),
        (0, 1, 1),
        "a round dialled in ends the silence and keeps its probe spent"
    );
    let rearmed = under(&VOUCH_REARMS, || dial_cadence_transition(&probed, "Vouch"));
    assert!(!model.successors("Vouch", &probed).contains(&rearmed));

    let mut vouched = model.init_state();
    for (var, value) in [
        ("beaconed", 1),
        ("vouched", 1),
        ("wait", 3),
        ("rest", 1),
        ("age", 1),
    ] {
        vouched.insert(var, value);
    }
    assert_eq!(
        dial_cadence_transition(&vouched, "Dial"),
        vouched,
        "a round dialled in puts the silence's probe off"
    );
    let unheard = under(&INBOUND_UNHEARD, || {
        dial_cadence_transition(&vouched, "Dial")
    });
    assert!(!model.check_invariant("VouchEndsSilence", &unheard));

    let mut owing = model.init_state();
    owing.insert("busy", 1);
    owing.insert("owes", 1);
    let rested = under(&RESTS_PENDING, || dial_cadence_transition(&owing, "Finish"));
    assert!(!model.check_invariant("PendingNeverRests", &rested));

    let mut broken = model.init_state();
    broken.insert("busy", 1);
    broken.insert("breaks", 1);
    let retried = dial_cadence_transition(&broken, "Finish");
    assert_eq!((retried["wait"], retried["rest"]), (0, 0), "{retried:?}");
    let rested = under(&UNFINISHED_FINISHES, || {
        dial_cadence_transition(&broken, "Finish")
    });
    assert!(!model.check_invariant("PendingNeverRests", &rested));

    let mut resting = model.init_state();
    resting.insert("wait", 3);
    resting.insert("rest", 1);
    let stirred = under(&REPULLS, || dial_cadence_transition(&resting, "Vouch"));
    assert!(!model.check_invariant("HealthyVouchInert", &stirred));

    let mut quiet = model.init_state();
    quiet.insert("busy", 1);
    let redialled = under(&QUIET_REDIALS, || dial_cadence_transition(&quiet, "Finish"));
    assert!(!model.successors("Finish", &quiet).contains(&redialled));

    let moved = dial_cadence_transition(&model.init_state(), "Move");
    let walked = |mut state: State| {
        while state["hot"] == 1 && state["walked"] <= 3 {
            state = dial_cadence_transition(&state, "WalkLacks");
        }
        state
    };
    let ended = walked(moved.clone());
    assert_eq!((ended["hot"], ended["walked"]), (0, 3), "{ended:?}");
    let unending = under(&LED_UNSETTLED, || walked(moved));
    assert!(
        !model.check_invariant("WalkEnds", &unending),
        "{unending:?}"
    );

    let owing = dial_cadence_transition(&model.init_state(), "Owe");
    assert_eq!(owing["owed"], 1, "a short list owes the seat a round");
    let paid = dial_cadence_transition(&owing, "WalkOwed");
    assert_eq!((paid["owed"], paid["owedwalks"]), (0, 1), "{paid:?}");
    let unowed = under(&SHORT_UNOWED, || {
        dial_cadence_transition(&model.init_state(), "Owe")
    });
    assert_eq!(unowed["owed"], 0, "MUTANT: nothing owed");
    let kept = under(&OWED_KEPT, || {
        let once = dial_cadence_transition(&owing, "WalkOwed");
        dial_cadence_transition(&once, "WalkOwed")
    });
    assert!(
        !model.check_invariant("OwedWalksOnce", &kept),
        "MUTANT: {kept:?}"
    );
}
