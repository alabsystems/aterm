// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{Session, store::Store, tests::Lab};
use aterm_secrets::commons::{
    node::{Edit, Node},
    roster::Role,
};

/// Mac 0's first table store in `lab`, holding one record, and the session it writes with.
fn store<'a>(
    lab: &Lab,
    bootstrap: &'a aterm_secrets::commons::bootstrap::Bootstrap,
) -> (Session<'a>, Store) {
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(bootstrap).unwrap();
    let scope = session.scope().unwrap();
    let dir = directory.path(scope).unwrap();
    aterm_journal::create_private_dir_durable(dir.parent().unwrap()).unwrap();
    let mut store = Store::join(&session, &dir, scope).unwrap();
    store
        .commit_catalog(&session, b"held before the look")
        .unwrap();
    (session, store)
}

/// A look tells a table whose store holds another record, once, and a store first held after
/// the look began; what it held at its start is what later looks compare with, and tells
/// nothing. NEGATIVE CONTROL (run here): a look with no write between tells nothing.
#[test]
fn a_look_tells_a_table_that_holds_another_record_once() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let (session, mut first) = store(&lab, &bootstrap);
    let mut look = Look::new(&lab.paths[0], bootstrap.journal_dir(), bootstrap.trust());
    assert_eq!(
        look.moved(),
        BTreeSet::new(),
        "nothing moved since the look began"
    );
    first.commit_catalog(&session, b"written since").unwrap();
    let table = Subject::Table(first.scope().digest());
    assert_eq!(look.moved(), BTreeSet::from([table]));
    assert_eq!(look.moved(), BTreeSet::new(), "a move is told once");
    // A store this Mac first holds after the look began.
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = session.scope().unwrap();
    let mut second = Store::join(&session, &directory.path(scope).unwrap(), scope).unwrap();
    second.commit_catalog(&session, b"a new table").unwrap();
    assert_eq!(
        look.moved(),
        BTreeSet::from([Subject::Table(scope.digest())])
    );
}

/// A look tells the control journal once it holds another node, and the membership records
/// once their file holds other bytes. A records file replaced by one that holds the same bytes
/// (a new file, a new stamp) is no move: nothing rewritten without a change can keep a walk
/// going.
/// MUTANT (`STAMPS_TELL`): the same records rewritten read as a move.
#[test]
fn a_look_tells_the_control_journal_and_the_records_by_what_they_hold() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::locate_for_home(root.path());
    let journal_dir = paths.dir.join("control");
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&journal_dir)
        .unwrap();
    let seed = [21; 32];
    let recipient = aterm_secrets::age::Identity::generate()
        .unwrap()
        .recipient();
    let genesis = Node::test_seal(
        &seed,
        [9; 16],
        Edit::Create {
            hosts: &[],
            payload: b"sealed",
        },
        std::slice::from_ref(&recipient),
        0,
        &[],
    )
    .unwrap();
    let update = Node::test_seal(
        &seed,
        [9; 16],
        Edit::Update {
            parent: genesis.id(),
            hosts: &[],
            payload: b"new state",
        },
        std::slice::from_ref(&recipient),
        0,
        &[],
    )
    .unwrap();
    let trust = Trust {
        network: [9; 16],
        authority: crate::crypto::public(&seed).unwrap(),
        genesis: genesis.id(),
    };
    Journal::open(&journal_dir, trust)
        .unwrap()
        .ingest(genesis.bytes())
        .unwrap();
    std::fs::write(paths.records_file(), b"records as first read").unwrap();

    let mut look = Look::new(&paths, &journal_dir, trust);
    assert_eq!(look.moved(), BTreeSet::new());
    Journal::open_existing(&journal_dir, trust)
        .unwrap()
        .ingest(update.bytes())
        .unwrap();
    assert_eq!(look.moved(), BTreeSet::from([Subject::Control]));
    assert_eq!(look.moved(), BTreeSet::new(), "a move is told once");

    // The same records in a new file: another stamp, nothing moved.
    for mutant in [false, true] {
        aterm_secrets::fsx::write_atomic(&paths.records_file(), b"records as first read", 0o600)
            .unwrap();
        STAMPS_TELL.with(|flag| flag.set(mutant));
        let told = look.moved();
        STAMPS_TELL.with(|flag| flag.set(false));
        if mutant {
            assert_eq!(
                told,
                BTreeSet::from([Subject::Members]),
                "MUTANT: a rewrite moved it"
            );
        } else {
            assert_eq!(told, BTreeSet::new(), "the same records are no move");
        }
    }
    aterm_secrets::fsx::write_atomic(&paths.records_file(), b"records with one more", 0o600)
        .unwrap();
    assert_eq!(look.moved(), BTreeSet::from([Subject::Members]));
    std::fs::remove_file(paths.records_file()).unwrap();
    assert_eq!(
        look.moved(),
        BTreeSet::from([Subject::Members]),
        "the file went"
    );
    assert_eq!(look.moved(), BTreeSet::new());
}

/// A transport's look is taken only when one is due: its owner's polled round asked, or a round
/// received. Each move is told with the seats it was received from since the look before and
/// when each of those rounds began, and a seat named for something that did not move is not kept
/// for a later move.
/// NEGATIVE CONTROL (run here): with no look due, a write is not told.
#[test]
fn a_look_is_taken_when_due_and_names_the_seats_a_move_came_from() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let (session, mut store) = store(&lab, &bootstrap);
    let table = Subject::Table(store.scope().digest());
    let mut moves = Moves::unwatched(&lab.paths[0], bootstrap.journal_dir(), bootstrap.trust());
    let mut write = |what: &[u8]| {
        store.commit_catalog(&session, what).unwrap();
    };
    write(b"one");
    assert_eq!(moves.take(), [], "no look is due");
    moves.poll();
    assert_eq!(moves.take(), [(table, vec![])], "the polled round's look");
    moves.poll();
    assert_eq!(moves.take(), [], "told once");
    write(b"two");
    let began = std::time::Instant::now();
    moves.received(Subject::Control, [3; 48], began);
    moves.received(table, [4; 48], began);
    assert_eq!(
        moves.take(),
        [(table, vec![([4; 48], began)])],
        "a round received it, and when that round began"
    );
    write(b"three");
    moves.poll();
    assert_eq!(
        moves.take(),
        [(table, vec![])],
        "an earlier look's seats are not kept"
    );
    // A transport given nothing to look at tells nothing.
    let mut idle = Moves::default();
    idle.poll();
    idle.received(table, [4; 48], std::time::Instant::now());
    assert_eq!(idle.take(), []);
}

/// A record appended to a table store rings the doorbell the watch was given, and the look that
/// ring asks for tells the table: the serving loop hears a write without waiting for its polled
/// round. The watch tells the claim directory once as it first looks at it; the append is the
/// second time, so the ring this waits for is the append's.
#[test]
fn an_append_rings_the_doorbell_and_the_look_it_asks_for_tells_the_table() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let (session, mut store) = store(&lab, &bootstrap);
    let bell = Arc::new(Doorbell::new().unwrap());
    let mut moves = Moves::start(
        &lab.paths[0],
        bootstrap.journal_dir(),
        bootstrap.trust(),
        Some(bell.clone()),
    );
    let told = |moves: &Moves, count: usize| {
        let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while moves.claims_told() < count {
            assert!(
                std::time::Instant::now() < until,
                "the watch told {count} times"
            );
            aterm_uds::deadline::poll_yield();
        }
    };
    told(&moves, 1);
    bell.clear();
    moves.take();
    store
        .commit_catalog(&session, b"written while watched")
        .unwrap();
    told(&moves, 2);
    assert!(bell.clear(), "the watch rang the doorbell");
    assert_eq!(
        moves.take(),
        [(Subject::Table(store.scope().digest()), vec![])],
        "the look the ring asked for"
    );
}
