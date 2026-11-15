// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The append hook and the lockless catch-up reader (one-mesh design §11 B7b): one append to a
//! table store yields exactly one sink record, after its witness, tagged with where it came from;
//! another process's append is read back from a cursor, and never past the witness.

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::{client::Paths, commons::roster::Role};
use std::{cell::RefCell, rc::Rc};

/// What the sink saw of one append, with the witness count its claim held at that moment.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    table: Digest,
    recovery: bool,
    kind: AppendKind,
    offset: u64,
    length: u64,
    via: Via,
    wire: Vec<u8>,
    witnessed: u64,
}

/// Run `run` with this thread's appends recorded; each record also notes the witness count the
/// appending store's claim (`claim`: directory, name) held on disk when the sink ran.
fn recorded<T>(claim: (PathBuf, String), run: impl FnOnce() -> T) -> (T, Vec<Seen>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let out = with_append_sink(
        move |record| {
            let (dir, name) = &claim;
            let claim = private_file::read_private(dir, name, CLAIM_LEN).unwrap();
            sink.borrow_mut().push(Seen {
                table: record.table,
                recovery: record.recovery,
                kind: record.kind,
                offset: record.offset,
                length: record.length,
                via: record.via,
                wire: record.wire.to_vec(),
                witnessed: Witness::decode(&claim[WITNESS_AT..]).unwrap().count,
            });
        },
        run,
    );
    let seen = seen.take();
    (out, seen)
}

fn claim_of(store: &Store) -> (PathBuf, String) {
    (store.claim_dir.clone(), store.claim_name.clone())
}

#[test]
fn one_append_yields_one_sink_record_after_its_witness_and_a_duplicate_none() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let mut writer = Store::create(&sa, &lab._home.path().join("table-a")).unwrap();
    let scope = writer.scope();
    let mut reader = Store::join(&sb, &lab._home.path().join("table-b"), scope).unwrap();
    let (writes, reads) = (claim_of(&writer), claim_of(&reader));

    // A local commit.
    let (receipt, seen) = recorded(writes, || writer.commit_catalog(&sa, b"one row").unwrap());
    let wire = writer.wire(&sa, &receipt.id).unwrap().unwrap().to_vec();
    let local = Seen {
        table: scope.digest(),
        recovery: false,
        kind: AppendKind::Operation,
        offset: receipt.offset,
        length: receipt.offset + 1,
        via: Via::Local,
        wire: wire.clone(),
        witnessed: receipt.offset + 1,
    };
    assert_eq!(
        seen,
        [local],
        "a local commit is one Local record, after its witness"
    );

    // An old-path ingest (a repair round's) is Local too; its duplicate fires nothing.
    let (old_path, seen) = recorded(reads.clone(), || reader.ingest(&sb, &wire).unwrap());
    assert_eq!(old_path.offset, 0);
    assert_eq!(
        seen,
        [Seen {
            table: scope.digest(),
            recovery: false,
            kind: AppendKind::Operation,
            offset: 0,
            length: 1,
            via: Via::Local,
            wire: wire.clone(),
            witnessed: 1,
        }],
        "an old-path ingest is one Local record"
    );
    let (again, seen) = recorded(reads.clone(), || {
        reader.ingest_via(&sb, &wire, Via::Mesh).unwrap()
    });
    assert_eq!(again, old_path, "a duplicate answers its stored receipt");
    assert!(
        seen.is_empty(),
        "a duplicate appends nothing and fires nothing: {seen:?}"
    );

    // A record the mesh delivered is tagged Mesh.
    let second = writer.commit_catalog(&sa, b"two rows").unwrap();
    let second_wire = writer.wire(&sa, &second.id).unwrap().unwrap().to_vec();
    let (mesh, seen) = recorded(reads, || {
        reader.ingest_via(&sb, &second_wire, Via::Mesh).unwrap()
    });
    assert_eq!(mesh.offset, 1);
    assert_eq!(
        seen,
        [Seen {
            table: scope.digest(),
            recovery: false,
            kind: AppendKind::Operation,
            offset: 1,
            length: 2,
            via: Via::Mesh,
            wire: second_wire,
            witnessed: 2,
        }],
        "a mesh ingest is one Mesh record"
    );
}

#[test]
fn a_crash_tail_a_writing_open_adopts_yields_one_sink_record() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let mut writer = Store::create(&sa, &lab._home.path().join("table-a")).unwrap();
    let scope = writer.scope();
    let dir = lab._home.path().join("table-b");
    let reads = {
        let mut reader = Store::join(&sb, &dir, scope).unwrap();
        let first = writer.commit_catalog(&sa, b"witnessed").unwrap();
        let wire = writer.wire(&sa, &first.id).unwrap().unwrap().to_vec();
        reader.ingest(&sb, &wire).unwrap();
        claim_of(&reader)
    };
    // A writer cut off between its journal append and its witness publish.
    let tail = writer.commit_catalog(&sa, b"the tail").unwrap();
    let wire = writer.wire(&sa, &tail.id).unwrap().unwrap().to_vec();
    let claim = private_file::read_private(&reads.0, &reads.1, CLAIM_LEN).unwrap();
    {
        let mut log = Log::open(
            &dir,
            "table",
            &claim[..ANCHOR_LEN],
            true,
            Capacity::default().journal(),
        )
        .unwrap();
        let mut body = tail.received_ms.to_le_bytes().to_vec();
        body.extend_from_slice(&wire);
        log.append(SUBJECT, body).unwrap();
    }
    let (reopened, seen) = recorded(reads.clone(), || Store::open(&sb, &dir, scope).unwrap());
    assert_eq!(
        seen,
        [Seen {
            table: scope.digest(),
            recovery: false,
            kind: AppendKind::Operation,
            offset: 1,
            length: 2,
            via: Via::Local,
            wire,
            witnessed: 2,
        }],
        "the adopted tail is one Local record, after its witness"
    );
    drop(reopened);
    let (_, seen) = recorded(reads, || Store::open(&sb, &dir, scope).unwrap());
    assert!(
        seen.is_empty(),
        "a reopen with nothing to adopt fires nothing: {seen:?}"
    );
}

/// The child's environment: the lab Mac's home, the table and its generation.
const CHILD_HOME: &str = "ATERM_TEST_HELD_HOME";
const CHILD_TABLE: &str = "ATERM_TEST_HELD_TABLE";
const CHILD_GENERATION: &str = "ATERM_TEST_HELD_GENERATION";
const CHILD_RESULT: &str = "held-child-appended:";

#[test]
#[ignore = "child process fixture, invoked by read_held_reads_another_process_append_and_never_past_the_witness"]
fn child_appends_one_record() {
    let home = std::env::var_os(CHILD_HOME).unwrap();
    let paths = Paths::for_home(Path::new(&home));
    let mut table = [0; 16];
    assert!(aterm_codec::hex::decode_into(
        std::env::var(CHILD_TABLE).unwrap().as_bytes(),
        &mut table
    ));
    let generation = std::env::var(CHILD_GENERATION).unwrap().parse().unwrap();
    let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::cached(&paths)
        .unwrap()
        .unwrap();
    let scope = Scope::new(bootstrap.trust(), table, generation).unwrap();
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&paths, bootstrap.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let receipt = store
        .commit_catalog(&session, b"from another process")
        .unwrap();
    println!(
        "{CHILD_RESULT}{}:{}",
        receipt.offset,
        aterm_codec::hex::encode_lower(&receipt.id)
    );
}

#[test]
fn read_held_reads_another_process_append_and_never_past_the_witness() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let (scope, first) = {
        let session = Session::open(&bootstrap).unwrap();
        let scope = session.scope().unwrap();
        let dir = directory.path(scope).unwrap();
        aterm_journal::create_private_dir_durable(dir.parent().unwrap()).unwrap();
        let mut store = Store::join(&session, &dir, scope).unwrap();
        (scope, store.commit_catalog(&session, b"before").unwrap())
    };
    let table = scope.digest();
    // The reader finds the store where the store itself keeps it.
    let (claims, data) = held::dirs_of(paths, false);
    assert_eq!(claims, claim_dir_of(&bootstrap).unwrap());
    assert_eq!(
        data.join(aterm_codec::hex::encode_lower(&table)),
        directory.path(scope).unwrap()
    );
    assert_eq!(held_lengths(paths).unwrap(), [(table, false, 1)]);
    let cursor = first.offset + 1;

    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "table::store::hook_tests::child_appends_one_record",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_HOME, &paths.home)
        .env(CHILD_TABLE, aterm_codec::hex::encode_lower(scope.table()))
        .env(CHILD_GENERATION, scope.generation().to_string());
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (offset, id) = stdout
        .lines()
        .find_map(|line| line.split_once(CHILD_RESULT).map(|(_, value)| value))
        .and_then(|value| value.split_once(':'))
        .expect("the child reported its append");
    assert_eq!(offset.parse::<u64>().unwrap(), cursor);

    assert_eq!(held_lengths(paths).unwrap(), [(table, false, 2)]);
    let held = read_held(paths, table, false, cursor, u64::MAX).unwrap();
    let session = Session::observe(&bootstrap).unwrap();
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let mut child_id = [0; 32];
    assert!(aterm_codec::hex::decode_into(id.as_bytes(), &mut child_id));
    assert_eq!(
        held,
        [Held {
            offset: cursor,
            kind: AppendKind::Operation,
            wire: store.wire(&session, &child_id).unwrap().unwrap().to_vec(),
        }],
        "exactly the other process's record, from the cursor"
    );
    assert_eq!(read_held(paths, table, false, 0, 1).unwrap().len(), 1);
    assert!(
        read_held(paths, table, false, 2, u64::MAX)
            .unwrap()
            .is_empty()
    );
    drop(store);
    drop(session);

    // An append whose witness was never published (a writer cut off between the two) is in the
    // journal, and stays out of what the reader hands on.
    let dir = directory.path(scope).unwrap();
    let claim = private_file::read_private(&claims, &claim_name(scope), CLAIM_LEN).unwrap();
    {
        let mut log = Log::open(
            &dir,
            "table",
            &claim[..ANCHOR_LEN],
            true,
            Capacity::default().journal(),
        )
        .unwrap();
        let mut body = 0u64.to_le_bytes().to_vec();
        body.extend_from_slice(b"an unwitnessed tail");
        log.append(SUBJECT, body).unwrap();
    }
    let journal = match aterm_journal::read_committed(
        &dir,
        "table",
        &claim[..ANCHOR_LEN],
        Capacity::default().journal(),
        None,
    )
    .unwrap()
    {
        aterm_journal::Committed::Rebuilt { records, .. } => records,
        _ => panic!("a first read rebuilds"),
    };
    assert_eq!(
        journal.len(),
        3,
        "the tail is in the journal (negative control)"
    );
    let held = read_held(paths, table, false, 0, u64::MAX).unwrap();
    assert_eq!(
        held.iter().map(|held| held.offset).collect::<Vec<_>>(),
        [0, 1],
        "nothing past the witness"
    );
    assert_eq!(held_lengths(paths).unwrap(), [(table, false, 2)]);
}

/// A reader that takes no lock (`held_lengths`, whose witness lengths the egress's catch-up
/// relays up to) never finds a witness of a record its journal has yet to commit: at the
/// commit's full flush the claim still witnesses the store's old length, and the receipt's once
/// the commit returned, so no record leaves the Mac that a power loss can still take.
/// NEGATIVE CONTROL: with the witness written ahead of the commit (`Mutant::WitnessFirst`),
/// the reader at the commit finds the new length.
#[test]
fn a_lock_free_reader_finds_no_witness_ahead_of_its_commit() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = session.scope().unwrap();
    let dir = directory.path(scope).unwrap();
    aterm_journal::create_private_dir_durable(dir.parent().unwrap()).unwrap();
    let mut store = Store::join(&session, &dir, scope).unwrap();
    store.commit_catalog(&session, b"first").unwrap();
    let table = scope.digest();
    for mutant in [
        aterm_journal::seam::Mutant::None,
        aterm_journal::seam::Mutant::WitnessFirst,
    ] {
        let length = store.witness.count;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (readings, reader) = (Rc::clone(&seen), paths.clone());
        let hook = aterm_journal::seam::before_each_commit(move || {
            readings.borrow_mut().push(held_lengths(&reader).unwrap());
        });
        let mutate = aterm_journal::seam::mutate(mutant);
        store.commit_catalog(&session, b"next").unwrap();
        drop((mutate, hook));
        let at_commit = if mutant == aterm_journal::seam::Mutant::WitnessFirst {
            length + 1
        } else {
            length
        };
        assert_eq!(
            *seen.borrow(),
            [vec![(table, false, at_commit)]],
            "{mutant:?}"
        );
        assert_eq!(held_lengths(paths).unwrap(), [(table, false, length + 1)]);
    }
}

/// A claim directory that cannot be listed fails `held_length` as it fails `held_lengths`, so
/// `Mesh::push_table` takes its fallback instead of reading the store as empty; one that is
/// absent holds no store.
#[test]
fn held_length_fails_where_the_claim_directory_cannot_be_listed() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let table = [7; 32];
    let (claims, _) = held::dirs_of(paths, false);
    if claims.is_dir() {
        std::fs::remove_dir_all(&claims).unwrap();
    }
    assert_eq!(held_length(paths, table, false).unwrap(), None);
    std::fs::create_dir_all(claims.parent().unwrap()).unwrap();
    std::fs::write(&claims, b"not a directory").unwrap();
    assert!(held_lengths(paths).is_err());
    assert!(held_length(paths, table, false).is_err());
}
