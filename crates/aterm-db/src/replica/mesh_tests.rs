// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mesh's ingest (one-mesh design §4.6, §11 B7b): a delivered record passes the checks a
//! repair batch runs, with no channel peer; one whose writer this Mac has not seated waits and
//! applies after its admission arrives.

use super::*;
use crate::{
    catalog::Directory,
    table::{
        store::{AppendKind, with_append_sink},
        tests::Lab,
    },
};
use aterm_secrets::{
    client,
    commons::{
        binding::Pending,
        bootstrap::Device,
        journal::Journal,
        roster::{Grant, Role},
    },
};
use std::{cell::RefCell, rc::Rc};

fn replica(lab: &Lab, n: usize) -> Replica {
    Replica::commons_until(
        lab.paths[n].clone(),
        lab.paths[n].home.join("config.toml"),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
}

/// Every operation wire Mac `n` holds of `scope`, by digest.
fn held(lab: &Lab, n: usize, scope: Scope) -> BTreeMap<Digest, Vec<u8>> {
    let bootstrap = lab.bootstrap(n);
    let session = Session::observe(&bootstrap).unwrap();
    let store = Directory::new(&lab.paths[n], bootstrap.trust())
        .observe_table(&session, scope)
        .unwrap();
    let ids: Vec<Digest> = store.inventory(&session).unwrap().copied().collect();
    ids.into_iter()
        .map(|id| (id, store.wire(&session, &id).unwrap().unwrap().to_vec()))
        .collect()
}

/// `run` with this thread's appends recorded as (table, kind, via).
fn appends<T>(run: impl FnOnce() -> T) -> (T, Vec<(Digest, AppendKind, Via)>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let out = with_append_sink(
        move |record| {
            sink.borrow_mut()
                .push((record.table, record.kind, record.via));
        },
        run,
    );
    let seen = seen.take();
    (out, seen)
}

#[test]
fn a_delivered_operation_applies_once_and_bad_or_unselected_records_are_refused() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], a.trust());
    let catalog = directory.catalog_scope();
    let scope = {
        let session = Session::open(&a).unwrap();
        directory
            .open_application(&session, "mesh-ingest", "rows", 1)
            .unwrap()
    };
    let descriptors = held(&lab, 0, catalog);
    assert_eq!(descriptors.len(), 1);
    let (id, wire) = descriptors.first_key_value().unwrap();

    let b = replica(&lab, 1);
    assert!(b.mesh_tables().unwrap().contains(&catalog.digest()));
    let (first, seen) = appends(|| {
        b.mesh_ingest(
            catalog.digest(),
            Kind::Operation,
            std::slice::from_ref(wire),
        )
        .unwrap()
    });
    let [Verdict::Applied(receipt)] = first.as_slice() else {
        panic!("the descriptor applies: {first:?}");
    };
    assert_eq!(receipt.id, *id);
    assert_eq!(
        seen,
        [(catalog.digest(), AppendKind::Operation, Via::Mesh)],
        "a delivered record is appended once, tagged Mesh"
    );
    let (again, seen) = appends(|| {
        b.mesh_ingest(
            catalog.digest(),
            Kind::Operation,
            std::slice::from_ref(wire),
        )
        .unwrap()
    });
    assert_eq!(
        again, first,
        "the same record again answers the same receipt"
    );
    assert!(seen.is_empty(), "and appends nothing: {seen:?}");
    assert_eq!(
        b.mesh_inventory(catalog.digest()).unwrap(),
        Some(BTreeSet::from([*id]))
    );
    assert_eq!(
        b.mesh_wires(catalog.digest(), &[*id, [3; 32]]).unwrap(),
        std::slice::from_ref(wire)
    );

    // A bad wire: a flipped signature byte, and bytes that are no operation at all.
    let mut forged = wire.clone();
    *forged.last_mut().unwrap() ^= 1;
    let bad = b
        .mesh_ingest(
            catalog.digest(),
            Kind::Operation,
            &[forged, b"not an operation".to_vec()],
        )
        .unwrap();
    assert!(
        bad.iter()
            .all(|verdict| matches!(verdict, Verdict::Refused(_))),
        "{bad:?}"
    );
    // A table this Mac neither holds nor subscribes.
    assert!(matches!(
        b.mesh_ingest([7; 32], Kind::Operation, std::slice::from_ref(wire))
            .unwrap()
            .as_slice(),
        [Verdict::Refused(_)]
    ));
    assert_eq!(b.mesh_inventory([7; 32]).unwrap(), None);
    assert!(
        b.mesh_ingest(
            catalog.digest(),
            Kind::Operation,
            &vec![wire.clone(); BATCH + 1]
        )
        .is_err()
    );

    // The descriptor that entered subscribes this Mac to its table; a row of it enters too.
    assert!(b.mesh_tables().unwrap().contains(&scope.digest()));
    {
        let session = Session::open(&a).unwrap();
        directory
            .open_store(&session, scope)
            .unwrap()
            .commit_catalog(&session, b"a row")
            .unwrap();
    }
    let rows: Vec<Vec<u8>> = held(&lab, 0, scope).into_values().collect();
    let applied = b
        .mesh_ingest(scope.digest(), Kind::Operation, &rows)
        .unwrap();
    assert!(
        matches!(applied.as_slice(), [Verdict::Applied(_)]),
        "{applied:?}"
    );
}

/// A ring window's table one Mac wrote and another has no storage for (§11 B8, the mesh's
/// one-sided tables): the other's selection lists it not, and a row of it is refused, until
/// `mesh_takes` takes it up as a repair round takes up one a peer offers; then the selection lists
/// it and the row applies. A table outside the window, or of no ring, is not taken.
#[test]
fn a_ring_table_a_mac_never_held_is_taken_up_by_mesh_takes_and_its_row_then_applies() {
    let now = aterm_time::unix_millis().unwrap() / 1000;
    for ring in [
        &crate::catalog::ring::CLAIMS,
        &crate::catalog::ring::RECOVERIES,
        &crate::catalog::ring::JOURNAL,
    ] {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let (table, wire) = {
            let a = lab.bootstrap(0);
            let session = Session::open(&a).unwrap();
            let directory = Directory::new(&lab.paths[0], a.trust());
            let epoch = ring.epoch_of(now);
            let mut store = directory.enroll_ring(&session, ring, epoch, now).unwrap();
            let receipt = store.commit_own(&session, b"a ring row").unwrap();
            let wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
            (ring.scope(directory.trust(), epoch).unwrap().digest(), wire)
        };
        let mut b = replica(&lab, 1);
        b.ring_clock = Some(now);
        assert!(!b.mesh_tables().unwrap().contains(&table), "{}", ring.role);
        assert!(
            matches!(
                b.mesh_ingest(table, Kind::Operation, std::slice::from_ref(&wire))
                    .unwrap()
                    .as_slice(),
                [Verdict::Refused(_)]
            ),
            "a row of a ring table B has not taken up is refused ({})",
            ring.role
        );
        let trust = lab.bootstrap(1).trust();
        let old = ring.scope(trust, ring.epoch_of(now) - 10).unwrap().digest();
        assert!(!b.mesh_takes(old).unwrap(), "a period outside the window");
        assert!(!b.mesh_takes([7; 32]).unwrap(), "a table of no ring");
        assert!(b.mesh_takes(table).unwrap(), "{}", ring.role);
        assert!(b.mesh_tables().unwrap().contains(&table), "{}", ring.role);
        let applied = b
            .mesh_ingest(table, Kind::Operation, std::slice::from_ref(&wire))
            .unwrap();
        assert!(
            matches!(applied.as_slice(), [Verdict::Applied(_)]),
            "{}: {applied:?}",
            ring.role
        );
        assert!(b.mesh_takes(table).unwrap(), "a selected table is taken");
    }
}

/// Admit every Mac of `lab` as `roles`, with the rotation entered into the control journals of
/// `told` alone; the rotation's bytes, to tell the others later.
fn admit_telling(lab: &Lab, roles: &[Role], told: &[usize]) -> Vec<u8> {
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let bootstraps: Vec<_> = (0..lab.paths.len()).map(|n| lab.bootstrap(n)).collect();
    let journal =
        Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
    let grants = roles
        .iter()
        .enumerate()
        .map(|(n, role)| {
            let device = Device::open(&lab.paths[n]).unwrap().unwrap();
            let request = bootstraps[n].request().unwrap();
            let (mut pending, challenge) = Pending::issue(
                &request,
                *request.binding().network(),
                *request.binding().authority(),
            )
            .unwrap();
            let possession = pending
                .verify(&device.answer(&request, &challenge).unwrap())
                .unwrap();
            let certificate = admin
                .approve_commons_member(&journal, &format!("m{}", n + 1), possession)
                .unwrap();
            Grant::certified(format!("m{}", n + 1), certificate, *role)
        })
        .collect();
    let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
    drop(journal);
    for &n in told {
        tell(lab, n, node.bytes());
    }
    node.bytes().to_vec()
}

fn tell(lab: &Lab, n: usize, node: &[u8]) {
    let bootstrap = lab.bootstrap(n);
    Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
        .unwrap()
        .ingest(node)
        .unwrap();
}

#[test]
fn a_record_whose_writer_is_not_seated_here_waits_then_applies_after_its_admission() {
    let lab = Lab::with_enrolled(&[Role::Writer; 3], 2);
    // Mac 3 is admitted; Mac 1 and Mac 3 hear of it, Mac 2 not yet.
    let rotation = admit_telling(&lab, &[Role::Writer; 3], &[0, 2]);
    let late = lab.bootstrap(2);
    let catalog = {
        let session = Session::open(&late).unwrap();
        let directory = Directory::new(&lab.paths[2], late.trust());
        directory
            .open_application(&session, "late-writer", "rows", 1)
            .unwrap();
        directory.catalog_scope()
    };
    let wires: Vec<Vec<u8>> = held(&lab, 2, catalog).into_values().collect();
    assert_eq!(wires.len(), 1);

    let b = replica(&lab, 1);
    let waiting = b
        .mesh_ingest(catalog.digest(), Kind::Operation, &wires)
        .unwrap();
    assert!(
        matches!(waiting.as_slice(), [Verdict::Waiting(_)]),
        "a writer this Mac has not seated waits: {waiting:?}"
    );
    // Negative control: untyped, the same record reads as refused for good.
    let untyped = crate::untyped_membership(|| {
        b.mesh_ingest(catalog.digest(), Kind::Operation, &wires)
            .unwrap()
    });
    assert!(
        matches!(untyped.as_slice(), [Verdict::Refused(_)]),
        "{untyped:?}"
    );

    tell(&lab, 1, &rotation);
    let applied = b
        .mesh_ingest(catalog.digest(), Kind::Operation, &wires)
        .unwrap();
    assert!(
        matches!(applied.as_slice(), [Verdict::Applied(_)]),
        "after its admission arrives it applies: {applied:?}"
    );
}

#[test]
fn a_peerless_admission_is_not_held_to_the_view_its_call_began_with() {
    let lab = Lab::with_enrolled(&[Role::Writer; 3], 2);
    let b = replica(&lab, 1);
    let peer = lab.bootstrap(0).request().unwrap().binding().actor();
    let round = b.round(None).unwrap();
    // This Mac's control history moves on after the call began.
    let rotation = admit_telling(&lab, &[Role::Writer; 3], &[]);
    tell(&lab, 1, &rotation);
    // Negative control: a round with a peer holds every admission to the view it probed.
    let pinned = round.local(peer, |_| Ok(())).unwrap_err();
    assert_eq!(
        pinned.to_string(),
        "control history changed during table repair"
    );
    round
        .local(None, |_| Ok(()))
        .expect("a peerless admission takes the view as it now is");
    round
        .local_write(None, |_| Ok(()))
        .expect("so does a peerless batch");
}
