// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted writers and encrypted transfers for wide-frontier application tests.

use super::Lab;
use crate::table::{Scope, Session, store::Store};
use aterm_secrets::{
    client,
    commons::{
        binding::Pending,
        hearing::{LIFTED, Ledger, Said},
        journal::Journal,
        roster::{Grant, LEGACY_MAX_MEMBERS, Role},
    },
};

pub(crate) fn wires(lab: &Lab, writer: usize, scope: Scope) -> Vec<Vec<u8>> {
    let bootstrap = lab.bootstrap(writer);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[writer], bootstrap.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    store
        .inventory(&session)
        .unwrap()
        .map(|id| store.wire(&session, id).unwrap().unwrap().to_vec())
        .collect()
}

pub(crate) fn receive(lab: &Lab, writer: usize, scope: Scope, wires: &[Vec<u8>]) {
    let bootstrap = lab.bootstrap(writer);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[writer], bootstrap.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    for wire in wires {
        store.ingest(&session, wire).unwrap();
    }
}

pub(crate) fn wide_lab(writers: usize) -> Lab {
    let lab = Lab::with_enrolled(&vec![Role::Writer; writers], LEGACY_MAX_MEMBERS);
    // Each writer's cached bootstrap, read once: its device, request, journal folder and trust
    // are what every step below needs of it, and none of them changes as the roster grows.
    let peers = (0..writers).map(|n| lab.bootstrap(n)).collect::<Vec<_>>();
    let requests = peers
        .iter()
        .map(|peer| peer.request().unwrap())
        .collect::<Vec<_>>();
    let owner = &peers[0];
    let mut ledger = Ledger::new(owner.trust());
    let now = aterm_time::unix_millis().unwrap();
    for request in &requests {
        ledger.record(request.binding().actor(), now, Said::Version(LIFTED));
    }
    ledger.write(owner).unwrap();
    {
        let admin = client::admin_open(&lab.paths[0]).unwrap();
        let recipients = lab.paths[LEGACY_MAX_MEMBERS..]
            .iter()
            .map(|paths| {
                client::machine_identity(paths)
                    .unwrap()
                    .recipient()
                    .to_bech32()
            })
            .collect::<Vec<_>>();
        client::admin_publish_approved(&lab.paths[0], &admin, &admin.doc, &recipients).unwrap();
    }
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let possession = |writer: usize| {
        let request = &requests[writer];
        let (mut pending, challenge) =
            Pending::issue(request, owner.trust().network, owner.trust().authority).unwrap();
        let answer = peers[writer].device().answer(request, &challenge).unwrap();
        pending.verify(&answer).unwrap()
    };
    let mut nodes = Vec::new();
    // The first two rosters past 63 members take production enrollment (enroll_commons_member):
    // the crossing, under its version gate, and the first growth of a roster already past 63,
    // the path by which every later device joins a network that crossed (batching it too would
    // leave a refusal there untested). A history that holds such a roster needs no gate, so the
    // other writers join in one rotation, each by its own proven and approved possession: at 129
    // writers every later open of every writer replays four rosters, not one more per writer
    // past 63 (67).
    let enrolled = writers.min(LEGACY_MAX_MEMBERS + 2);
    for writer in LEGACY_MAX_MEMBERS..enrolled {
        let node = admin
            .enroll_commons_member(owner, &mut journal, possession(writer))
            .unwrap();
        nodes.push(node.bytes().to_vec());
    }
    if writers > enrolled {
        let mut grants = requests[..enrolled]
            .iter()
            .enumerate()
            .map(|(n, request)| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    request.binding().clone(),
                    Role::Writer,
                )
            })
            .collect::<Vec<_>>();
        for writer in enrolled..writers {
            let host = format!("m{}", writer + 1);
            let certificate = admin
                .auto_approve_commons_member(&journal, &host, possession(writer))
                .unwrap();
            grants.push(Grant::certified(host, certificate, Role::Writer));
        }
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        let access = owner.access_in(&journal).unwrap().unwrap();
        assert_eq!(access.members().len(), writers, "every writer is enrolled");
        nodes.push(node.bytes().to_vec());
    }
    drop(journal);
    // Every peer receives the same admitted sequence through one open of its journal.
    for peer in &peers[1..] {
        let mut journal = Journal::open_existing(peer.journal_dir(), peer.trust()).unwrap();
        for node in &nodes {
            journal.ingest(node).unwrap();
        }
    }
    lab
}
