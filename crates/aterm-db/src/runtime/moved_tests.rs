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

/// Where a look under `paths` reads the Commons configuration's stamp ([`Look::held_since`]):
/// no file until a test writes one.
fn config(paths: &Paths) -> PathBuf {
    paths.dir.join("commons-policy.toml")
}

/// A look tells a table whose store holds another record, once, and a store first held after
/// the look began; what it held at its start is what later looks compare with, and tells
/// nothing. NEGATIVE CONTROL (run here): a look with no write between tells nothing.
#[test]
fn a_look_tells_a_table_that_holds_another_record_once() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let (session, mut first) = store(&lab, &bootstrap);
    let mut look = Look::new(
        &lab.paths[0],
        bootstrap.journal_dir(),
        &config(&lab.paths[0]),
        bootstrap.trust(),
    );
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

/// A control journal holding its genesis alone, under a scratch home, and the nodes appended to
/// it since ([`Self::append`]).
struct Control {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    dir: PathBuf,
    trust: Trust,
    seed: [u8; 32],
    recipient: aterm_secrets::age::Recipient,
    last: Digest,
}

impl Control {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::locate_for_home(root.path());
        let dir = paths.dir.join("control");
        std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(&dir)
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
        let trust = Trust {
            network: [9; 16],
            authority: crate::crypto::public(&seed).unwrap(),
            genesis: genesis.id(),
        };
        Journal::open(&dir, trust)
            .unwrap()
            .ingest(genesis.bytes())
            .unwrap();
        Self {
            _root: root,
            paths,
            dir,
            trust,
            seed,
            recipient,
            last: genesis.id(),
        }
    }

    /// Append a node that updates the last one with `payload`.
    fn append(&mut self, payload: &[u8]) {
        let update = Node::test_seal(
            &self.seed,
            [9; 16],
            Edit::Update {
                parent: self.last,
                hosts: &[],
                payload,
            },
            std::slice::from_ref(&self.recipient),
            0,
            &[],
        )
        .unwrap();
        Journal::open_existing(&self.dir, self.trust)
            .unwrap()
            .ingest(update.bytes())
            .unwrap();
        self.last = update.id();
    }
}

/// A look tells the control journal once it holds another node, and the membership records
/// once their file holds other bytes. A records file replaced by one that holds the same bytes
/// (a new file, a new stamp) is no move: nothing rewritten without a change can keep a walk
/// going.
/// MUTANT (`STAMPS_TELL`): the same records rewritten read as a move.
#[test]
fn a_look_tells_the_control_journal_and_the_records_by_what_they_hold() {
    let mut control = Control::new();
    let (paths, journal_dir, trust) = (control.paths.clone(), control.dir.clone(), control.trust);
    std::fs::write(paths.records_file(), b"records as first read").unwrap();

    let mut look = Look::new(&paths, &journal_dir, &config(&paths), trust);
    assert_eq!(look.moved(), BTreeSet::new());
    control.append(b"new state");
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
/// for a later move. A table grown by more than its rounds brought moved by something no seat
/// sent as well, and names none of them.
/// NEGATIVE CONTROL (run here): with no look due, a write is not told.
/// MUTANT (`SOURCES_HOLD_ALL`): the seat is named for the table grown past what it brought.
#[test]
fn a_look_is_taken_when_due_and_names_the_seats_a_move_came_from() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let (session, mut store) = store(&lab, &bootstrap);
    let table = Subject::Table(store.scope().digest());
    let mut moves = Moves::unwatched(
        &lab.paths[0],
        bootstrap.journal_dir(),
        &config(&lab.paths[0]),
        bootstrap.trust(),
    );
    let mut write = |what: &[u8]| {
        store.commit_catalog(&session, what).unwrap();
    };
    write(b"one");
    assert_eq!(moves.take(Instant::now(), None), [], "no look is due");
    moves.poll();
    assert_eq!(
        moves.take(Instant::now(), None),
        [(table, Source::Here)],
        "the polled round's look"
    );
    moves.poll();
    assert_eq!(moves.take(Instant::now(), None), [], "told once");
    // Each round here began after the last look.
    let began = Instant::now();
    write(b"two");
    moves.received(Subject::Control, [3; 48], began, 1);
    moves.received(table, [4; 48], began, 1);
    assert_eq!(
        moves.take(Instant::now(), None),
        [(table, Source::Seats(vec![[4; 48]]))],
        "a round received it"
    );
    write(b"three");
    moves.poll();
    assert_eq!(
        moves.take(Instant::now(), None),
        [(table, Source::Here)],
        "an earlier look's seats are not kept"
    );
    for mutant in [false, true] {
        let began = Instant::now();
        write(b"four");
        write(b"five");
        moves.received(table, [4; 48], began, 1);
        SOURCES_HOLD_ALL.with(|flag| flag.set(mutant));
        let told = moves.take(Instant::now(), None);
        SOURCES_HOLD_ALL.with(|flag| flag.set(false));
        if mutant {
            assert_eq!(
                told,
                [(table, Source::Seats(vec![[4; 48]]))],
                "MUTANT: the seat"
            );
        } else {
            assert_eq!(told, [(table, Source::Here)], "two records, one brought");
        }
    }
    // A transport given nothing to look at tells nothing.
    let mut idle = Moves::default();
    idle.poll();
    idle.received(table, [4; 48], Instant::now(), 1);
    assert_eq!(idle.take(Instant::now(), None), []);
}

/// The control journal's move names its seats by the same rule as a table's: a journal grown by
/// more nodes than its rounds added since the last look moved by a node written here too, which
/// those seats may lack, and names none of them; one grown by no more names the seat.
/// MUTANT (`SOURCES_HOLD_ALL`): the seat is named for the journal grown past what it added, as
/// it is where a look records no growth of the control journal.
#[test]
fn a_control_journal_grown_past_what_its_rounds_added_names_no_seat() {
    let mut control = Control::new();
    let mut moves = Moves::unwatched(
        &control.paths,
        &control.dir,
        &config(&control.paths),
        control.trust,
    );
    for (round, mutant) in [false, true].into_iter().enumerate() {
        // Each round here began after the last look.
        let began = Instant::now();
        control.append(format!("written here {round}").as_bytes());
        control.append(format!("received {round}").as_bytes());
        moves.received(Subject::Control, [3; 48], began, 1);
        SOURCES_HOLD_ALL.with(|flag| flag.set(mutant));
        let told = moves.take(Instant::now(), None);
        SOURCES_HOLD_ALL.with(|flag| flag.set(false));
        if mutant {
            assert_eq!(
                told,
                [(Subject::Control, Source::Seats(vec![[3; 48]]))],
                "MUTANT: the seat"
            );
        } else {
            assert_eq!(
                told,
                [(Subject::Control, Source::Here)],
                "two nodes, one added"
            );
        }
    }
    let began = Instant::now();
    control.append(b"received alone");
    moves.received(Subject::Control, [3; 48], began, 1);
    assert_eq!(
        moves.take(Instant::now(), None),
        [(Subject::Control, Source::Seats(vec![[3; 48]]))],
        "one node, one added"
    );
}

/// A round that began before an earlier look found its subject moved is no source of a later
/// move: that look ran while the round was in flight, and keeps when it found the subject moved
/// while that round is ([`Moves::take`]'s `oldest`). A table's record a round brought is in the
/// store, and a look finds it before the round's receipt comes; a write here follows, and the
/// receipt of one record comes before the next look, which finds the table grown by one, the
/// write. The receipt says nothing of that: the look names no seat. Of the control journal, a
/// node a round in flight brought is found by a look before its receipt; then a later round
/// brings another node, and the next look names that round's seat alone, though the first
/// round's receipt came before it too.
/// MUTANT (`STALE_SOURCES_HOLD`): the late receipt covers the write, and the first round's seat
/// is named beside the later round's.
#[test]
fn a_round_in_flight_at_a_look_that_found_its_subject_moved_is_no_source_of_a_later_move() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let (session, mut store) = store(&lab, &bootstrap);
        let table = Subject::Table(store.scope().digest());
        let mut moves = Moves::unwatched(
            &lab.paths[0],
            bootstrap.journal_dir(),
            &config(&lab.paths[0]),
            bootstrap.trust(),
        );
        let began = Instant::now();
        store.commit_catalog(&session, b"brought").unwrap();
        moves.poll();
        // The round that brought it is still in flight at this look.
        assert_eq!(
            moves.take(Instant::now(), Some(began)),
            [(table, Source::Here)],
            "the look finds the record before its round's receipt"
        );
        store.commit_catalog(&session, b"written here").unwrap();
        moves.received(table, [4; 48], began, 1);
        STALE_SOURCES_HOLD.with(|flag| flag.set(mutant));
        let told = moves.take(Instant::now(), None);
        STALE_SOURCES_HOLD.with(|flag| flag.set(false));
        if mutant {
            assert_eq!(
                told,
                [(table, Source::Seats(vec![[4; 48]]))],
                "MUTANT: the write covered"
            );
        } else {
            assert_eq!(told, [(table, Source::Here)], "the write is no seat's");
        }

        let mut control = Control::new();
        let mut moves = Moves::unwatched(
            &control.paths,
            &control.dir,
            &config(&control.paths),
            control.trust,
        );
        let stale = Instant::now();
        control.append(b"brought by a round in flight");
        moves.poll();
        assert_eq!(
            moves.take(Instant::now(), Some(stale)),
            [(Subject::Control, Source::Here)],
            "the look finds the node before its round's receipt"
        );
        let fresh = Instant::now();
        control.append(b"brought by a later round");
        moves.received(Subject::Control, [3; 48], stale, 1);
        moves.received(Subject::Control, [5; 48], fresh, 1);
        STALE_SOURCES_HOLD.with(|flag| flag.set(mutant));
        let told = moves.take(Instant::now(), None);
        STALE_SOURCES_HOLD.with(|flag| flag.set(false));
        if mutant {
            assert_eq!(
                told,
                [(Subject::Control, Source::Seats(vec![[3; 48], [5; 48]]))],
                "MUTANT: both seats"
            );
        } else {
            assert_eq!(
                told,
                [(Subject::Control, Source::Seats(vec![[5; 48]]))],
                "the later round's seat"
            );
        }
    }
}

/// A look keeps when it found each subject moved only while a round that began before then is in
/// flight, since only such a round's receipt is judged by it ([`Moves::take`]): a look with no
/// round in flight keeps none, one with a round in flight that began before it keeps its
/// subject's, and a later look once that round was joined keeps none again. However many tables
/// move over a transport's life (ring tables rotate each period), what it keeps is bounded by
/// what moved since its oldest round in flight began. That round's receipt is still judged by
/// the look it was in flight at (`a_round_in_flight_at_a_look_that_found_its_subject_moved_is_no_source_of_a_later_move`).
/// MUTANT (`MOVES_KEPT`): every subject that ever moved keeps its entry.
#[test]
fn a_look_keeps_when_it_found_a_subject_moved_only_while_an_earlier_round_is_in_flight() {
    use std::time::Duration;
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let (session, mut store) = store(&lab, &bootstrap);
        let table = Subject::Table(store.scope().digest());
        let mut moves = Moves::unwatched(
            &lab.paths[0],
            bootstrap.journal_dir(),
            &config(&lab.paths[0]),
            bootstrap.trust(),
        );
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut look = |write: &[u8], now: Instant, oldest: Option<Instant>| {
            store.commit_catalog(&session, write).unwrap();
            moves.poll();
            MOVES_KEPT.with(|flag| flag.set(mutant));
            let told = moves.take(now, oldest);
            MOVES_KEPT.with(|flag| flag.set(false));
            assert_eq!(told, [(table, Source::Here)], "MUTANT {mutant}");
            moves.moved_at.len()
        };
        let idle = look(b"one", at(1), None);
        let in_flight = look(b"two", at(3), Some(at(2)));
        let joined = look(b"three", at(4), None);
        assert_eq!(
            in_flight, 1,
            "kept while a round that began before the look is in flight"
        );
        if mutant {
            assert_eq!((idle, joined), (1, 1), "MUTANT: kept");
        } else {
            assert_eq!(
                (idle, joined),
                (0, 0),
                "no round in flight began before the look"
            );
        }
    }
}

/// A changed Commons configuration counts each store here as taken up at the look that finds the
/// change ([`Look::held_since`]): its policy may select again a table it excluded, whose store
/// this Mac kept. A look that finds the configuration as it was keeps each store's instant, and
/// a change is no move.
/// MUTANT (`CONFIG_UNSTAMPED`): the change restamps nothing.
#[test]
fn a_changed_configuration_counts_each_store_taken_up_anew() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let (_session, first) = store(&lab, &bootstrap);
        let digest = first.scope().digest();
        let config = config(&lab.paths[0]);
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
        let mut look = Look::new(
            &lab.paths[0],
            bootstrap.journal_dir(),
            &config,
            bootstrap.trust(),
        );
        let found = look.held_since(&digest).expect("the first look found it");
        assert_eq!(look.moved(), BTreeSet::new());
        assert_eq!(
            look.held_since(&digest),
            Some(found),
            "an unchanged configuration restamps nothing"
        );
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\nexclude_tables = []\n", 0o600)
            .unwrap();
        let changed = Instant::now();
        CONFIG_UNSTAMPED.with(|flag| flag.set(mutant));
        let told = look.moved();
        CONFIG_UNSTAMPED.with(|flag| flag.set(false));
        assert_eq!(told, BTreeSet::new(), "a changed configuration is no move");
        let kept = look.held_since(&digest).unwrap();
        if mutant {
            assert_eq!(kept, found, "MUTANT: the store keeps its first instant");
        } else {
            assert!(
                kept >= changed,
                "the store counts as taken up at the change"
            );
        }
    }
}

/// A look keeps when a look first found each store ([`Look::held_since`]): the first look's
/// stores bear its instant, a store first found by a later look that look's, and neither moves
/// as later looks read the stores again or find them written. That age is what tells a store
/// this Mac held before a round's lists from one it took up since
/// (`crate::replica::Listing::takes`). A table no look found has none.
/// MUTANT (`HELD_RESTAMPED`): each look stamps the stores it reads anew, so the first store
/// looks as young as the last look.
#[test]
fn a_look_keeps_when_it_first_found_each_store() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let (session, mut first) = store(&lab, &bootstrap);
        let digest = first.scope().digest();
        let before = Instant::now();
        let mut look = Look::new(
            &lab.paths[0],
            bootstrap.journal_dir(),
            &config(&lab.paths[0]),
            bootstrap.trust(),
        );
        let after = Instant::now();
        let found = look.held_since(&digest).expect("the first look found it");
        assert!(
            before <= found && found <= after,
            "the first look's instant"
        );
        HELD_RESTAMPED.with(|flag| flag.set(mutant));
        first.commit_catalog(&session, b"written since").unwrap();
        assert_eq!(look.moved(), BTreeSet::from([Subject::Table(digest)]));
        let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
        let scope = session.scope().unwrap();
        let mut second = Store::join(&session, &directory.path(scope).unwrap(), scope).unwrap();
        second.commit_catalog(&session, b"a new table").unwrap();
        let later = Instant::now();
        assert_eq!(
            look.moved(),
            BTreeSet::from([Subject::Table(scope.digest())])
        );
        look.moved();
        HELD_RESTAMPED.with(|flag| flag.set(false));
        let kept = look.held_since(&digest).unwrap();
        if mutant {
            assert!(kept > found, "MUTANT: the first store restamped");
            continue;
        }
        assert_eq!(
            kept, found,
            "the first store keeps the first look's instant"
        );
        let young = look.held_since(&scope.digest()).unwrap();
        assert!(
            young >= later,
            "a store first found later bears that look's instant"
        );
        assert_eq!(look.held_since(&[0; 32]), None, "no look found it");
    }
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
        &config(&lab.paths[0]),
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
    moves.take(Instant::now(), None);
    store
        .commit_catalog(&session, b"written while watched")
        .unwrap();
    told(&moves, 2);
    assert!(bell.clear(), "the watch rang the doorbell");
    assert_eq!(
        moves.take(Instant::now(), None),
        [(Subject::Table(store.scope().digest()), Source::Here)],
        "the look the ring asked for"
    );
}
