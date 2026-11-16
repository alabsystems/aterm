// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A replica keeps each table's store across a session's batches and across rounds
//! ([`crate::table::store::kept`]): what a batch reopens instead of replaying, and what it judges
//! again at each permission boundary.

use super::*;
use crate::crypto::Causal;
use std::time::Instant;

/// How many opens on this thread replayed a store's whole journal while `run` ran
/// ([`crate::table::store::journal_replays`]).
fn replays<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = crate::table::store::journal_replays();
    let out = run();
    (out, crate::table::store::journal_replays() - before)
}

/// Mac 0's table of `old`, written before Mac 1 was admitted, which Mac 1 reads only through a
/// real history handoff from Mac 0, and `new`, Mac 1's own, which depends on nothing: Mac 1 holds
/// both.
fn historical_pair() -> (Pair, [(Digest, Vec<u8>); 2]) {
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Writer], 1);
    let paths = [
        lab._home.path().join("table-a"),
        lab._home.path().join("table-b"),
    ];
    let (scope, old) = {
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &paths[0]).unwrap();
        let id = store.commit_catalog(&session, b"historical").unwrap().id;
        let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
        (store.scope(), (id, wire))
    };
    lab.admit(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    let handoff = {
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let access = a.access_in(&journal).unwrap().unwrap();
        a.history_handoff(
            &journal,
            &access,
            a.trust().genesis,
            b.request().unwrap().binding(),
        )
        .unwrap()
    };
    {
        let journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
        let access = b.access_in(&journal).unwrap().unwrap();
        b.import_history(&journal, &access, handoff.bytes())
            .unwrap();
    }
    let session = Session::open(&b).unwrap();
    let mut store = Store::join(&session, &paths[1], scope).unwrap();
    let id = store.commit_catalog(&session, b"current").unwrap().id;
    let new = (id, store.wire(&session, &id).unwrap().unwrap().to_vec());
    store.ingest(&session, &old.1).unwrap();
    drop((store, session));
    (Pair { lab, scope, paths }, [old, new])
}

/// A store a replica keeps is judged again at each permission boundary against the evidence
/// now, not only its control head: Mac 1's history handoff for `old` moved away, with its
/// control head unchanged, withholds `old` on the reused round path (an outgoing batch that
/// selects it restarts, the inventory does not offer it, and its pending header leaves the
/// eligible projection); moved back, with the control head still unchanged, `old` is offered
/// and sent again. And a second round over a table that did not change replays no store: each
/// of its opens reopens what the replica kept.
/// NEGATIVE CONTROLS: with `Verdict::same` comparing the discarded-source verdict alone (a
/// cache keyed by the control head), the batch selecting `old` is sent with its history gone:
/// `` must restart: Ok(2) ``; with `KEPT_STORES` 0 (nothing kept), each read replays the store:
/// `` a kept store is reopened, not replayed left: 1 right: 0 ``.
#[test]
fn a_kept_store_rejudges_eligibility_at_each_boundary_and_a_quiet_round_replays_nothing() {
    let (pair, [old, new]) = historical_pair();
    let table = pair.scope.digest();
    let replica = pair.replica(1);
    let peer = pair.lab.bootstrap(0).request().unwrap().binding().actor();
    let ids = [old.0, new.0];
    let wires = |ids: &[Digest]| {
        let round = replica.round(peer).unwrap();
        round.local(peer, |session| {
            round.prepare_operations(session, table, ids)
        })
    };
    let inventory = || {
        let round = replica.round(peer).unwrap();
        round
            .inventory(peer, table, &mut None, &mut None)
            .unwrap()
            .unwrap()
    };
    let eligible = || {
        let round = replica.round(peer).unwrap();
        round
            .local(peer, |session| {
                let store = round.open(session, table)?;
                let eligible: BTreeSet<_> =
                    store.replication_inventory_unchecked().copied().collect();
                round.keep(store);
                Ok(eligible)
            })
            .unwrap()
    };
    let b = pair.lab.bootstrap(1);
    let control = || match Journal::read_existing(b.journal_dir(), b.trust())
        .unwrap()
        .state()
    {
        aterm_secrets::commons::journal::State::Ready { head, basis } => (head, basis),
        state => panic!("control must remain ready: {state:?}"),
    };

    let both = vec![old.1.clone(), new.1.clone()];
    assert_eq!(wires(&ids).unwrap(), both);
    let (again, replayed) = replays(|| wires(&ids));
    assert_eq!(again.unwrap(), both);
    assert_eq!(replayed, 0, "a kept store is reopened, not replayed");

    let before = control();
    let aside = pair.lab._home.path().join("held-history");
    std::fs::create_dir(&aside).unwrap();
    let moved: Vec<_> = std::fs::read_dir(pair.lab.paths[1].dir.join("commons/history"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.path(), aside.join(entry.file_name()))
        })
        .collect();
    assert!(!moved.is_empty(), "a real historical handoff is required");
    for (from, to) in &moved {
        std::fs::rename(from, to).unwrap();
    }
    assert_eq!(control(), before);
    let refused = wires(&ids).map(|wires| wires.len());
    assert!(
        refused.as_ref().is_err_and(Error::is_snapshot_changed),
        "an outgoing batch selecting the record whose history went must restart: {refused:?}"
    );
    assert_eq!(wires(&[new.0]).unwrap(), std::slice::from_ref(&new.1));
    let offered = inventory();
    assert!(offered.known.contains(&old.0), "physical records stay held");
    assert!(!offered.offers(&old.0) && offered.offers(&new.0));
    assert_eq!(
        eligible(),
        BTreeSet::from([new.0]),
        "a pending historical header cannot stay in the eligible projection"
    );
    {
        let session = Session::observe(&b).unwrap();
        let error = session
            .authenticate(pair.scope, &old.1)
            .err()
            .expect("the moved handoff must actually be required");
        assert!(error.is_awaiting_membership(), "{error}");
    }
    assert_eq!(control(), before);

    for (from, to) in &moved {
        std::fs::rename(to, from).unwrap();
    }
    assert_eq!(
        wires(&ids).unwrap(),
        both,
        "restored history is eligible again"
    );
    assert!(inventory().offers(&old.0));
    assert_eq!(eligible(), BTreeSet::from(ids));
    assert_eq!(control(), before);

    // Rounds on `atdb/7`, Mac 1 initiating on this thread: the first may open afresh, the second,
    // over a table neither side changed, reopens what Mac 1 kept.
    let other = pair.replica(0);
    let reconciling = || Repair {
        reconcile: true,
        ..Repair::default()
    };
    let mut repairs = [reconciling(), reconciling()];
    for report in rbsr_round(&replica, &other, &mut repairs) {
        report.unwrap();
    }
    let (reports, replayed) = replays(|| rbsr_round(&replica, &other, &mut repairs));
    for report in reports {
        let report = report.unwrap();
        assert_eq!((report.sent, report.received), (0, 0), "{report:?}");
    }
    assert_eq!(replayed, 0, "a quiet round reopens what it kept");
}

/// A table of `held + moved` records by four writers on Mac 0, of which Mac 1 holds the first
/// `held`: a round moves the other `moved`.
fn large_pair(held: usize, moved: usize) -> Pair {
    const WRITERS: usize = 4;
    let lab = Lab::new(&[Role::Writer; WRITERS]);
    let bootstraps: Vec<_> = (0..WRITERS).map(|n| lab.bootstrap(n)).collect();
    let sessions: Vec<_> = bootstraps
        .iter()
        .map(|bootstrap| Session::open(bootstrap).unwrap())
        .collect();
    let paths = [
        lab._home.path().join("table-a"),
        lab._home.path().join("table-b"),
    ];
    let mut a = Store::create(&sessions[0], &paths[0]).unwrap();
    let scope = a.scope();
    let mut previous = [None; WRITERS];
    let wires: Vec<_> = (0..held + moved)
        .map(|row| {
            let writer = row % WRITERS;
            let wire = crate::table::tests::seal(
                &sessions[writer],
                scope,
                Causal {
                    sequence: u64::try_from(row / WRITERS + 1).unwrap(),
                    previous: previous[writer],
                    parents: previous[writer].as_slice(),
                },
                b"row",
            )
            .unwrap();
            previous[writer] = Some(Sha256::digest(&wire));
            wire
        })
        .collect();
    let ingest = |store: &mut Store, session: &Session<'_>, wires: &[Vec<u8>]| {
        for batch in wires.chunks(aterm_journal::MAX_APPEND_RECORDS) {
            let borrowed: Vec<_> = batch.iter().map(Vec::as_slice).collect();
            store.ingest_batch(session, &borrowed).unwrap();
        }
    };
    ingest(&mut a, &sessions[0], &wires);
    drop(a);
    let mut b = Store::join(&sessions[1], &paths[1], scope).unwrap();
    ingest(&mut b, &sessions[1], &wires[..held]);
    drop((b, sessions));
    Pair { lab, scope, paths }
}

/// One repair session on `atdb/7`, over TLS and under the exchange's own deadline (`EXCHANGE`, 60
/// s in tests), moves 2,000 operations into a table of 8,192, the most records a table store
/// holds (`table::store::MAX_RECORDS`): each of its 125 batches reopens the store each side keeps
/// and commits its records as one group, where each batch replayed every record the table holds,
/// on both sides, and 2,000 operations into a table of 6,000 took 70.7 s. Kept and grouped, that
/// session took 8.5 s; what each batch still pays in proportion to the table is the receiving
/// writer's journal open, which LT-B5R2 takes on with a table of 15,000 once LT-B10C2 lifts the
/// record bound.
/// NEGATIVE CONTROL: with `KEPT_STORES` 0 (each batch opens the store afresh, replaying the
/// table on both sides), the responder passes the deadline and drops the link mid-session:
/// `` application_error: Some(Failure { cause: Io, … peer closed connection ``.
#[test]
fn one_session_moves_2000_operations_into_a_full_table_within_the_exchange_deadline() {
    const MOVED: usize = 2_000;
    const HELD: usize = crate::table::store::MAX_RECORDS - MOVED;
    let pair = large_pair(HELD, MOVED);
    let (a, b) = (pair.endpoint(0, true), pair.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    let started = Instant::now();
    pair.round(&a, &b, &mut ra, &mut rb);
    let took = started.elapsed();
    for repair in [&ra, &rb] {
        assert!(repair.application_error().is_none(), "{repair:?}");
    }
    let sent = ra
        .application()
        .expect("the initiator's application round completed");
    let received = rb
        .application()
        .expect("the responder's application round completed");
    assert_eq!((sent.sent, received.received), (MOVED, MOVED));
    assert!(sent.caught_up, "{sent:?}");
    assert_eq!(pair.count(1), HELD + MOVED);
    println!(
        "{MOVED} operations into a table of {}: one session in {took:?}",
        HELD + MOVED
    );
}
