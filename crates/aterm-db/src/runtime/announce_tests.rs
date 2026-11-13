// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The announce bank and its listener, read as counts: what a turn would take after each
//! append, through the production store and the production listener.

use super::*;
use crate::table::{
    Session,
    store::{self, Store},
    tests::Lab,
};
use aterm_secrets::commons::roster::Role;

fn table(n: u8) -> Digest {
    [n; 32]
}

/// A table noted twice before the owner looks is taken once; what is taken is gone; and a
/// ninth table before the owner looks is kept as an append to an unnamed table, beside the
/// eight named.
#[test]
fn a_table_is_noted_once_and_a_ninth_is_kept_unnamed() {
    let bank = Announced::default();
    assert_eq!(bank.take(), None, "nothing noted");
    bank.note(&table(1));
    bank.note(&table(1));
    bank.note(&table(2));
    assert_eq!(
        bank.take(),
        Some((BTreeSet::from([table(1), table(2)]), false))
    );
    assert_eq!(bank.take(), None, "what a turn took is gone");
    for n in 0..9 {
        bank.note(&table(n));
    }
    let (tables, unnamed) = bank.take().expect("nine noted");
    assert_eq!(tables, (0..8).map(table).collect());
    assert!(unnamed, "the ninth table is kept unnamed");
    assert_eq!(bank.take(), None);
}

/// Two Macs' custodies in one process, each with the production listener on its own bank and
/// doorbell. A commit rings its own custody's owner once and names its table, and the other
/// custody's owner hears nothing of it. A record this process ingests on an ordinary thread (a
/// custody import, an old path) is announced as its own write is. What the mesh delivered
/// ([`Via::Mesh`]) and what a repair round's thread ingested ([`on_round`]) announce nothing.
///
/// NEGATIVE CONTROL (2026-11-12): with the listener noting the table and ringing nothing, the
/// commit's owner was not rung (`a commit rings its owner`: `left: (false, Some(({…}, false)))`,
/// `right: (true, Some(({…}, false)))`).
/// NEGATIVE CONTROL (2026-11-12): with the listener not reading whether its thread runs a round,
/// the round thread's ingest was announced (`a round's ingest announces nothing`: `left:
/// Some(({…}, false))`, `right: None`).
/// NEGATIVE CONTROL (2026-11-12): with `crate::table::store`'s listeners told of every custody's
/// appends, the other custody's owner was rung for the first commit (`another custody's commit
/// rings nothing here`: `left: (true, Some(({…}, false)))`, `right: (false, None)`).
#[test]
fn the_listener_announces_what_this_process_authored_in_its_own_custody() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    let (sa, sb) = (Session::open(&a).unwrap(), Session::open(&b).unwrap());
    let mut writer = Store::create(&sa, &lab._home.path().join("table-a")).unwrap();
    let scope = writer.scope();
    let mut reader = Store::join(&sb, &lab._home.path().join("table-b"), scope).unwrap();
    let owners: [(Arc<Announced>, Arc<Doorbell>); 2] =
        std::array::from_fn(|_| (Arc::default(), Arc::new(Doorbell::new().unwrap())));
    let hear = |bootstrap, n: usize| {
        let (bank, bell) = &owners[n];
        store::listen(
            store::claim_dir_of(bootstrap).unwrap(),
            bank.listener(Some(bell.clone())),
        )
        .expect("a free listener slot")
    };
    let _hearing = [hear(&a, 0), hear(&b, 1)];
    // What an owner's next turn would find: whether it was rung, and what it would take.
    let woken = |n: usize| (owners[n].1.clear(), owners[n].0.take());
    let named = Some((BTreeSet::from([scope.digest()]), false));
    assert_eq!([woken(0), woken(1)], [(false, None), (false, None)]);

    let first = writer.commit_catalog(&sa, b"one row").unwrap();
    assert_eq!(woken(0), (true, named.clone()), "a commit rings its owner");
    assert_eq!(
        woken(1),
        (false, None),
        "another custody's commit rings nothing here"
    );

    let wire = writer.wire(&sa, &first.id).unwrap().unwrap().to_vec();
    reader.ingest_via(&sb, &wire, Via::Mesh).unwrap();
    assert_eq!(woken(1), (false, None), "the mesh's delivery is the mesh's");

    let second = writer.commit_catalog(&sa, b"two rows").unwrap();
    let wire = writer.wire(&sa, &second.id).unwrap().unwrap().to_vec();
    assert_eq!(woken(0), (true, named.clone()));
    reader.ingest(&sb, &wire).unwrap();
    assert_eq!(
        woken(1),
        (true, named),
        "an ingest on an ordinary thread is this process's write"
    );

    // This thread runs a round from here to its end (the test's own thread, which ends with it).
    let third = writer.commit_catalog(&sa, b"three rows").unwrap();
    let wire = writer.wire(&sa, &third.id).unwrap().unwrap().to_vec();
    assert!(woken(0).0);
    on_round();
    reader.ingest(&sb, &wire).unwrap();
    assert_eq!(woken(1).1, None, "a round's ingest announces nothing");
    assert!(!owners[1].1.clear(), "and rings nothing");
}
