// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault server takes a served root into a rewritten records file (tracker FL-148E4):
//! its peer pull at format 2 asks for the frame a pruned peer serves, takes the root with the
//! records into its store, and rewrites its records entry to the rooted store
//! (`records_storage::rewrite`); it reads that file back, appends to it, and recovers a
//! rewrite cut short at any step. Its write never drops a record another writer put in its
//! file, its look keeps what its slot held, and it takes nothing of a generation another
//! writer rewrote while it read it. A server whose mark names no format 2, or whose build reads
//! none, reads and writes its records file as before. A marked server whose build does not
//! read its mark's format serves no record, and the follow pull asks a peer refused at several
//! endpoints once and proves the digest it stated.

use super::follow_tests::{HANG, Seat, Server, VAULT, follows, two};
use super::*;
use crate::crossing::{FORMAT_ONE, FORMAT_TWO, Faults, RecordsBound};
use crate::membership_log::stored::KEPT_MAGIC;
use crate::membership_log::{
    Log, Reading as RecordsReading, Record, Store, encode_records, pruned_vault,
};
use crate::records_storage::rewrite::{self, CUT, Cut, DIES, Dies, Piece};

/// The records entry of `server`'s vault.
fn entry(server: &Server) -> PathBuf {
    server.root.path().join(format!("{VAULT}.records"))
}

/// The rewrite generation of `server`'s records entry and the bytes of the data file a reader
/// of format 2 selects there.
fn stored_file(server: &Server) -> (u64, Vec<u8>) {
    let custody = rewrite::data_path(&entry(server)).expect("the entry's data file");
    let bytes = std::fs::read(&custody.path).expect("the data file");
    (custody.generation, bytes)
}

/// Every file and folder under `root`, by its path there, with each file's bytes.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
        for found in std::fs::read_dir(at).expect("the folder") {
            let path = found.expect("an entry").path();
            let name = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            if path.is_dir() {
                into.insert(format!("{name}/"), Vec::new());
                walk(root, &path, into);
            } else {
                into.insert(name, std::fs::read(&path).expect("the file"));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

/// The files and folders of `server`'s vault under its store folder (its records entry, its
/// mark and their lock files), by [`tree`]: not the server's own files, such as its heard
/// coverage, which records the second each start began.
fn vault_files(server: &Server) -> BTreeMap<String, Vec<u8>> {
    let mut files = tree(server.root.path());
    files.retain(|name, _| name.starts_with(&format!("{VAULT}.")));
    files
}

/// A server whose build reads format 2, its mark naming format 2, that holds the pruned peer's
/// store (`pruned_vault`) in a rewritten records file, started anew on that file.
fn pruned_server(peer: &Store) -> Server {
    let server = Server::new(follows());
    server.mark().store(FORMAT_TWO).expect("the mark");
    server
        .running()
        .test_persist_records(VAULT, peer, None, None)
        .expect("the rooted store written");
    let server = server.restarted();
    assert_eq!(
        server.store().heads_digest(),
        peer.heads_digest(),
        "the server reads its rewritten file as the pruned store"
    );
    server
}

/// A peer pull that wants `VAULT`'s records alone.
fn wants() -> [Want; 1] {
    [Want {
        vault: VAULT.to_owned(),
        due: Due {
            envelope: false,
            records: true,
        },
        named_seq: None,
    }]
}

/// A vault server behind a format-2 vault's final checkpoint catches up from a pruned server
/// and takes the root it serves into a rewritten records file. The pruned server reads its own
/// rewritten file (written through `persist_records`, then read anew). A server whose build
/// reads no format 2 asks once, asks for no served frame, and keeps its file. One whose build
/// reads it is refused at format 1, asks again stating format 2 and asking for a served frame,
/// takes the records and the root on a seat's version-2 record among them (its mark first),
/// and rewrites its entry: generation 1, the frame of roots then the records, and the slot
/// knows the file holds exactly its log. Its store is the pruned peer's: rooted at the
/// checkpoint, the sleeper's record held, the dropped record known and not held. Started anew
/// it reads the same store from the file. The pruned server takes the vault's next record by
/// an append to its file, and the follower pulls it by an append to its own. Its build then
/// made one that reads no format 2, it serves nothing (a `500`) and its entry is unchanged.
///
/// NEGATIVE CONTROLS (2026-11-18), each a change to the code run against this test and seen
/// to fail:
/// - `transport::fetch_record_frames_with_known` asking for no served frame (`served: false`,
///   as before this part): ``a pass that follows``, `left: 0` against `right: 1`: the follow
///   pull's records alone leave the sleeper's record waiting on the dropped one, no evidence.
/// - `persist_stored` answering `Stored::Ordinary` for every store (the ordinary writer writes
///   a rooted store, its frame lost): ``the server reads its rewritten file as the pruned
///   store`` in the seeding, two different heads digests.
/// - `look_at_records_with` reading a rewritten file as records (the frame test answering no):
///   the same message, `left: None` against the peer's digest.
#[test]
fn vault_checkpoint_a_server_takes_a_served_root_into_a_rewritten_file() {
    let vault = pruned_vault();
    let source = pruned_server(&vault.peer);
    let (generation, seeded) = stored_file(&source);
    assert_eq!(generation, 1);
    assert!(seeded.starts_with(KEPT_MAGIC));
    let asked = || {
        (
            source.running().records_gets(),
            source.running().records_served_gets(),
        )
    };

    // A build that reads no format 2: one ask, no served frame, nothing taken.
    let dark = Server::holding(Faults::default(), &vault.behind);
    let before = (dark.file(), asked());
    let _ = dark.running().pull_from_peers(&[source.as_peer()]);
    let after = asked();
    assert_eq!(
        (after.0 - before.1.0, after.1 - before.1.1),
        (1, 0),
        "a build that reads no format 2 asks once, for no served frame"
    );
    assert!(!dark.mark().path().exists());
    assert_eq!(dark.file(), before.0);
    drop(dark);

    // A build that reads it follows, and takes the root into a rewritten file.
    let puller = Server::holding(follows(), &vault.behind);
    let flat = puller.file();
    let before = asked();
    assert_eq!(
        puller.running().pull_from_peers(&[source.as_peer()]),
        1,
        "a pass that follows"
    );
    let after = asked();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1),
        (2, 1),
        "refused at format 1, then a served frame asked for once"
    );
    assert_eq!(puller.format(), FORMAT_TWO);
    let (generation, file) = stored_file(&puller);
    assert_eq!(generation, 1, "the entry is a rewritten layout");
    assert!(
        file.starts_with(KEPT_MAGIC),
        "its file begins with the frame"
    );
    assert_ne!(file, flat);
    let check = |store: &Store| {
        let log = store.log().expect("a log");
        assert_eq!(log.newest_root().map(|root| root.id), Some(vault.root));
        assert!(
            log.get(&vault.slept).is_some(),
            "the sleeper's record is held"
        );
        assert!(log.is_dropped(&vault.dropped) && log.get(&vault.dropped).is_none());
        assert_eq!(store.heads_digest(), vault.peer.heads_digest());
        assert_eq!(store.inventory(), vault.peer.inventory());
        assert_eq!(
            store.inventory(),
            store.complete_inventory(),
            "nothing waits"
        );
        log.held_count()
    };
    let held = check(&puller.store());
    assert_eq!(
        puller.running().test_records_on_file(VAULT),
        Some(held),
        "the slot knows its file holds exactly its log"
    );
    let puller = puller.restarted();
    assert_eq!(check(&puller.store()), held);
    assert_eq!(puller.running().test_records_on_file(VAULT), Some(held));
    assert_eq!(stored_file(&puller), (1, file.clone()));

    // The vault's next record: an append at the pruned server, and at the follower.
    let wire = vault.next.wire_bytes().expect("the record's bytes");
    let posted = source.post_two(&[&vault.next]);
    assert_eq!(posted.status, 204, "{}", posted.head);
    assert_eq!(
        stored_file(&source),
        (1, [&seeded[..], &wire[..]].concat()),
        "the pruned server appends"
    );
    let before = asked();
    assert_eq!(puller.running().pull_from_peers(&[source.as_peer()]), 1);
    let after = asked();
    assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
    assert_eq!(
        stored_file(&puller),
        (1, [&file[..], &wire[..]].concat()),
        "the follower appends"
    );
    assert!(puller.store().holds(vault.next.id()));

    // A build that reads no format 2 on the marked server serves nothing and writes nothing.
    puller.suffers(Faults::default());
    let refused = puller.get(&two());
    assert_eq!(refused.status, 500, "{}", refused.head);
    puller.suffers(follows());
    assert_eq!(stored_file(&puller), (1, [&file[..], &wire[..]].concat()));
}

/// A server whose mark names format 2 holds what a Mac behind the vault's final cut holds, and
/// the sleeper's record that follows only the record the cut dropped, which waits there for a
/// parent no peer holds any more. Its pull of the pruned server takes the root that peer
/// serves with its records: the sleeper's record is then complete, nothing waits, and its
/// entry is rewritten to the rooted store, whose file holds no waiting record.
///
/// NEGATIVE CONTROL (2026-11-18): `transport::fetch_record_frames_with_known` asking for no
/// served frame (`served: false`, as before this part), run against this test and seen to
/// fail with ``the root completes it``: the sleeper's record still waited on the dropped one.
#[test]
fn vault_checkpoint_a_server_behind_a_cut_holds_nothing_waiting_once_it_took_the_root() {
    let vault = pruned_vault();
    let source = pruned_server(&vault.peer);
    let slept = vault
        .peer
        .record(&vault.slept)
        .cloned()
        .expect("the sleeper's record");

    let behind = Server::new(follows());
    behind.mark().store(FORMAT_TWO).expect("the mark");
    let held: Vec<&Record> = vault.behind.iter().collect();
    assert_eq!(behind.post_two(&held).status, 204);
    assert_eq!(behind.post_two(&[&slept]).status, 204);
    let store = behind.store();
    assert!(
        store.contains(&vault.slept) && !store.holds(&vault.slept),
        "the sleeper's record waits on the dropped one"
    );

    assert_eq!(behind.running().pull_from_peers(&[source.as_peer()]), 1);
    let store = behind.store();
    assert!(store.holds(&vault.slept), "the root completes it");
    assert_eq!(
        store.inventory(),
        store.complete_inventory(),
        "nothing waits once the root is taken"
    );
    assert_eq!(store.heads_digest(), vault.peer.heads_digest());
    let (generation, file) = stored_file(&behind);
    assert_eq!(generation, 1);
    assert!(file.starts_with(KEPT_MAGIC));
    let behind = behind.restarted();
    let store = behind.store();
    assert_eq!(store.inventory(), store.complete_inventory());
    assert_eq!(store.heads_digest(), vault.peer.heads_digest());
}

/// Where a crash ends a server's rewrite of its records entry ([`DIES`]).
#[derive(Clone, Copy, Debug)]
enum Crash {
    /// Once the step is done.
    After(crate::records_storage::Step),
    /// At the cut ([`CUT`]).
    At(Cut),
}

/// A server whose mark names format 2 takes a served root into its records entry, and its
/// process dies at each step of the rewrite (after the intent, the copy, the preparation, the
/// pin and the exchange), at every scratch file (torn, flushed whole but not renamed, half lost
/// by the disk), with the pin's flush failed, and after each unlink. Started anew, it recovers
/// the entry and reads exactly the store it held before (the entry not rewritten: the records
/// file the format-1 layout holds, byte for byte) where the pin had not moved, and exactly the
/// rooted store (generation 1, the frame first) where it had. Either way it then takes the root
/// again and holds the rooted store.
///
/// NEGATIVE CONTROL (2026-11-18): `recover_records_entry` running the first transition's
/// recovery alone (`records_storage::recover_pending`) at format 2, as before this part, run
/// against this test and seen to fail with ``After(Intent): started anew it recovers the
/// entry``, `left: ["team.records.rewriting"]`: the rewrite's intent stood after the start.
#[test]
fn vault_checkpoint_a_server_crash_at_every_step_of_its_rewrite_recovers() {
    use crate::records_storage::Step;
    let vault = pruned_vault();
    let mut crashes: Vec<Crash> = [
        Step::Intent,
        Step::Copied,
        Step::Prepared,
        Step::Pinned,
        Step::Exchanged,
    ]
    .into_iter()
    .map(Crash::After)
    .collect();
    for piece in [Piece::Intent, Piece::Data, Piece::Header, Piece::Pin] {
        crashes.extend([
            Crash::At(Cut::Torn(piece)),
            Crash::At(Cut::Flushed(piece)),
            Crash::At(Cut::Lost(piece)),
        ]);
    }
    crashes.push(Crash::At(Cut::Unflushed));
    crashes.extend((1..=8).map(|unlinks| Crash::At(Cut::Unlinked(unlinks))));
    let (mut back, mut forward, mut unlinks) = (0, 0, 0);
    let mut whole_unlinks = None;
    for crash in crashes {
        let server = Server::new(follows());
        server.mark().store(FORMAT_TWO).expect("the mark");
        let held: Vec<&Record> = vault.behind.iter().collect();
        assert_eq!(server.post_two(&held).status, 204);
        let before = server.store();
        let flat = stored_file(&server);
        assert_eq!(flat.0, 0);
        let shared = &server.running().shared;
        let (stamp, on_file) = {
            let state = shared.lock_server_state();
            let slot = &state.records[VAULT];
            (slot.stamp, slot.on_file)
        };
        match crash {
            Crash::After(step) => DIES.set(Some(Dies::After(step))),
            Crash::At(cut) => {
                DIES.set(Some(Dies::AtCut));
                CUT.set(Some(cut));
            }
        }
        let crashed = server
            .running()
            .test_persist_records(VAULT, &vault.peer, stamp, on_file)
            .is_err();
        DIES.set(None);
        CUT.set(None);

        let server = server.restarted();
        // Its start recovered the entry: no rewrite intent and no scratch file of a rewrite is
        // left (a format-1 layout keeps its original and first intent beside it).
        let left: Vec<String> = tree(server.root.path())
            .into_keys()
            .filter(|name| name.ends_with(".rewriting") || name.ends_with(".building"))
            .collect();
        assert_eq!(
            left,
            Vec::<String>::new(),
            "{crash:?}: started anew it recovers the entry"
        );
        let store = server.store();
        let (generation, file) = stored_file(&server);
        let new = store.log().and_then(Log::newest_root).is_some();
        if new {
            assert_eq!(
                store.heads_digest(),
                vault.peer.heads_digest(),
                "{crash:?}: started anew it reads the rooted store"
            );
            assert_eq!(store.inventory(), vault.peer.inventory(), "{crash:?}");
            assert!(generation == 1 && file.starts_with(KEPT_MAGIC), "{crash:?}");
        } else {
            assert_eq!(
                (store.heads_digest(), store.inventory()),
                (before.heads_digest(), before.inventory()),
                "{crash:?}: started anew it reads the store it held"
            );
            assert_eq!((generation, file), flat, "{crash:?}: the file it held");
        }
        match crash {
            Crash::After(step) => {
                assert!(crashed, "{crash:?}");
                assert_eq!(
                    new,
                    matches!(step, Step::Pinned | Step::Exchanged),
                    "{crash:?}: started anew it reads the rooted store"
                );
            }
            Crash::At(Cut::Unlinked(_)) => {
                assert!(new, "{crash:?}");
                unlinks += usize::from(crashed);
            }
            Crash::At(Cut::Unflushed) => assert!(crashed && new, "{crash:?}"),
            Crash::At(_) => assert!(crashed && !new, "{crash:?}"),
        }
        if new {
            forward += 1;
        } else {
            back += 1;
        }

        // It takes the root again, and holds the rooted store.
        let shared = &server.running().shared;
        let (stamp, on_file) = {
            let state = shared.lock_server_state();
            let slot = &state.records[VAULT];
            (slot.stamp, slot.on_file)
        };
        rewrite::TRACE.with_borrow_mut(Vec::clear);
        server
            .running()
            .test_persist_records(VAULT, &vault.peer, stamp, on_file)
            .unwrap_or_else(|error| panic!("{crash:?}: the root taken again: {error}"));
        if !new {
            // The whole rewrite of a records file kept whole unlinks the original it keeps
            // aside, the predecessor's data and header, and the intents of the first
            // transition and of the rewrite.
            let unlinked = rewrite::TRACE.with_borrow(|trace| {
                trace
                    .iter()
                    .filter(|step| step.starts_with("unlink "))
                    .count()
            });
            whole_unlinks = Some(unlinked);
        }
        let server = server.restarted();
        assert_eq!(
            server.store().heads_digest(),
            vault.peer.heads_digest(),
            "{crash:?}"
        );
        assert_eq!(stored_file(&server).0, 1, "{crash:?}");
    }
    // Every unlink of the rewrite was a crash's place.
    assert_eq!(Some(unlinks), whole_unlinks);
    assert_eq!(unlinks, 5);
    assert!(back >= 15 && forward >= 9, "{back} back, {forward} forward");
}

/// A server whose mark names no format 2 reads and writes its records file exactly as before
/// this part, on a build that reads format 2 and on one that reads none: a store's write is its
/// held records alone (`Store::read_held`), a rooted store's among them, with no frame and no
/// rewritten layout; a look reads a records file as the records decoder reads it; a rewritten
/// entry is refused, nothing under the store folder changes, and the server holds nothing of
/// it; and its peer pull asks for no served frame.
///
/// NEGATIVE CONTROLS (2026-11-18), each a change to the code run against this test and seen
/// to fail:
/// - `persist_stored`'s format gate dropped (it runs for a store of any format): ``a rooted
///   store's write at format 1 is its held records alone``, on the build that reads no format
///   2: the entry became a rewritten layout (`team.records.layout` and its pin listed).
/// - `records_data_path` and the look's frame test run whatever the reading: ``an unmarked
///   server holds nothing of a rewritten entry``, on the build that reads no format 2.
#[test]
fn vault_checkpoint_an_unmarked_server_reads_and_writes_its_records_as_before() {
    let vault = pruned_vault();
    let mut seat = Seat::found();
    let base = seat.held.clone();
    let n1 = seat.sign(false);
    for faults in [Faults::default(), follows()] {
        // A whole store's write and read.
        let server = Server::holding(faults, &base);
        assert_eq!(
            server
                .post(None, &RecordsBound::uncrossed(), &encode_records([&n1]))
                .status,
            204
        );
        let file = server.file();
        let store = server.store();
        assert_eq!(file, store.read_held().expect("held"), "{faults:?}");
        let read = Store::for_vault(VAULT)
            .expect("a store")
            .read_at(RecordsReading::ONE)
            .recovering(&file);
        assert_eq!(read.inventory(), store.inventory(), "{faults:?}");
        assert_eq!(read.heads_digest(), store.heads_digest(), "{faults:?}");
        let server = server.restarted();
        assert_eq!(server.store().inventory(), store.inventory(), "{faults:?}");
        assert_eq!(
            server.running().test_records_on_file(VAULT),
            Some(store.log().map_or(0, Log::held_count)),
            "{faults:?}"
        );
        assert!(!server.mark().path().exists());

        // A rooted store, as no unmarked server holds one, is written as its held records.
        let rooted = vault.peer.clone().read_at(RecordsReading::ONE);
        let empty = Server::new(faults);
        empty
            .running()
            .test_persist_records(VAULT, &rooted, None, None)
            .expect("written");
        // The entry is one file (beside its writer's lock file): no layout, pin or intent.
        assert_eq!(
            tree(empty.root.path())
                .into_iter()
                .filter(|(name, _)| name.starts_with(&format!("{VAULT}.records"))
                    && *name != format!("{VAULT}.records.writing"))
                .collect::<Vec<_>>(),
            vec![(
                format!("{VAULT}.records"),
                rooted.read_held().expect("held")
            )],
            "{faults:?}: a rooted store's write at format 1 is its held records alone"
        );
    }

    // A rewritten entry, its mark gone: refused, and nothing changes.
    let mut marked = pruned_server(&vault.peer);
    std::fs::remove_file(marked.mark().path()).expect("the mark removed");
    let source = pruned_server(&vault.peer);
    for faults in [Faults::default(), follows()] {
        marked.suffers(faults);
        let before = vault_files(&marked);
        marked = marked.restarted();
        assert!(
            marked.store().log().is_none(),
            "{faults:?}: an unmarked server holds nothing of a rewritten entry"
        );
        let served = source.running().records_served_gets();
        let _ = marked.running().pull_from_peers(&[source.as_peer()]);
        assert_eq!(
            source.running().records_served_gets(),
            served,
            "{faults:?}: an unmarked server asks for no served frame"
        );
        assert_eq!(
            vault_files(&marked),
            before,
            "{faults:?}: nothing of the vault changes on disk"
        );
    }
}

/// The stamp of the file `server`'s records slot read, and how many of its store's records it
/// knows that file holds, after a look.
fn slot_read(server: &Server) -> (Option<fsx::FileStamp>, Option<usize>) {
    let _ = server.store();
    let state = server.running().shared.lock_server_state();
    let slot = &state.records[VAULT];
    (slot.stamp, slot.on_file)
}

/// `bytes` put on the end of the data file of `server`'s records entry, as another writer of
/// its store folder appends a record.
fn another_writer_appends(server: &Server, bytes: &[u8]) {
    let custody = rewrite::data_path(&entry(server)).expect("the entry's data file");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&custody.path)
        .expect("the data file")
        .write_all(bytes)
        .expect("the append");
}

/// A marked server's write of a store that reads format 2 never drops a record another writer
/// put in its records file after its slot read it (two servers may share a store folder). At
/// an entry never rewritten, another writer appends a record seat B signed, and the slot's
/// store then takes a served root: its rewrite is refused as the file changed since the slot
/// read it; asked with the file's current stamp, it is refused as the file holds a record the
/// store neither holds nor dropped. At a rewritten entry, another writer appends the vault's
/// next record to the generation's file, and the write onto that file with the slot's stamp is
/// refused. Each time the entry is what it was, and a look then takes the record.
///
/// NEGATIVE CONTROLS (2026-11-18), each a change to `persist_stored` run against this test and
/// seen to fail:
/// - The rewrite's check of the file's stamp against the slot's dropped: ``the file changed
///   since the slot read it``, the refusal naming a record the store was read without instead.
/// - The check that the store knows every record on file dropped: ``a store that lacks the
///   record on file does not replace it``, the rewrite answering `Ok`.
/// - The check of the generation's file stamp against the slot's dropped: ``the slot's stamp
///   of the generation's file is stale``, the write answering `Ok` (its append refused, the
///   file written whole without the record).
#[test]
fn vault_checkpoint_a_server_drops_no_record_another_writer_put_in_its_file() {
    let vault = pruned_vault();
    let changed = "changed since this writer read them";
    let unknown = "holds a record its store was read without";

    // An entry never rewritten: the rewrite at the served root.
    let server = Server::new(follows());
    server.mark().store(FORMAT_TWO).expect("the mark");
    let held: Vec<&Record> = vault.behind.iter().collect();
    assert_eq!(server.post_two(&held).status, 204);
    let (stamp, on_file) = slot_read(&server);
    another_writer_appends(
        &server,
        &vault.stray.wire_bytes().expect("the record's bytes"),
    );
    let before = vault_files(&server);
    let refused = server
        .running()
        .test_persist_records(VAULT, &vault.peer, stamp, on_file)
        .expect_err("the file changed since the slot read it");
    assert!(refused.to_string().contains(changed), "{refused}");
    assert_eq!(vault_files(&server), before, "the entry is what it was");
    let current = fsx::stamp(&rewrite::data_path(&entry(&server)).expect("selected").path);
    let refused = server
        .running()
        .test_persist_records(VAULT, &vault.peer, current, on_file)
        .expect_err("a store that lacks the record on file does not replace it");
    assert!(refused.to_string().contains(unknown), "{refused}");
    assert_eq!(vault_files(&server), before, "the entry is what it was");
    assert!(
        server.store().holds(vault.stray.id()),
        "a look takes the other writer's record"
    );

    // A rewritten entry: the write onto its generation's file.
    let server = pruned_server(&vault.peer);
    let (stamp, on_file) = slot_read(&server);
    another_writer_appends(
        &server,
        &vault.next.wire_bytes().expect("the record's bytes"),
    );
    let before = vault_files(&server);
    let refused = server
        .running()
        .test_persist_records(VAULT, &vault.peer, stamp, on_file)
        .expect_err("the slot's stamp of the generation's file is stale");
    assert!(refused.to_string().contains(changed), "{refused}");
    assert_eq!(vault_files(&server), before, "the entry is what it was");
    assert_eq!(stored_file(&server).0, 1);
    assert!(
        server.store().holds(vault.next.id()),
        "a look takes the other writer's record"
    );
}

/// A marked server that reads its rewritten file anew keeps what its slot held: a record that
/// waits in memory on the vault's next record (a server's file holds complete records alone)
/// is still there once another writer appends that record to the generation's file and the
/// server reads the file again, and is then complete.
///
/// NEGATIVE CONTROL (2026-11-18): `look_at_records_with` taking the rooted store it read in
/// place of the slot's (its union with the slot's records dropped), run against this test and
/// seen to fail with ``the record the slot held is kept, and complete``.
#[test]
fn vault_checkpoint_a_server_reading_its_rewritten_file_anew_keeps_what_it_held() {
    let vault = pruned_vault();
    let server = pruned_server(&vault.peer);
    let file = stored_file(&server);
    assert_eq!(server.post_two(&[&vault.after_next]).status, 204);
    let store = server.store();
    assert!(
        store.contains(vault.after_next.id()) && !store.holds(vault.after_next.id()),
        "the record waits on the next one"
    );
    assert_eq!(
        stored_file(&server),
        file,
        "a waiting record is not written"
    );
    another_writer_appends(
        &server,
        &vault.next.wire_bytes().expect("the record's bytes"),
    );
    let store = server.store();
    assert!(
        store.holds(vault.next.id()),
        "a look takes the other writer's record"
    );
    assert!(
        store.holds(vault.after_next.id()),
        "the record the slot held is kept, and complete"
    );
}

/// A server's look at a rewritten records entry that another writer rewrites while it reads
/// the selected generation's file takes nothing: the entry then selects another file, and the
/// bytes read are of a generation no longer pinned. Read again, it takes the new generation.
///
/// NEGATIVE CONTROL (2026-11-18): `look_at_records_with`'s check that the entry still selects
/// the file it read dropped, run against this test and seen to fail with ``a look whose entry
/// moved to another file while it was read fails``: the old generation's file, unlinked whole,
/// passed the reader's check that it did not change.
#[test]
fn vault_checkpoint_a_server_takes_nothing_of_a_generation_rewritten_while_it_read_it() {
    let vault = pruned_vault();
    let server = pruned_server(&vault.peer);
    let path = entry(&server);
    let (generation, file) = stored_file(&server);
    assert_eq!(generation, 1);
    let wire = vault.next.wire_bytes().expect("the record's bytes");
    let reading = RecordsReading::Format(FORMAT_TWO);
    let mut state = State::default();
    let outcome = look_at_records_with(&mut state, VAULT, &path, None, reading, || {
        rewrite::rewrite(
            &path,
            rewrite::Predecessor::of(&file),
            rewrite::What::Checkpoint(vault.root),
            &|out| {
                out.write_all(&file)?;
                out.write_all(&wire)
            },
        )
        .expect("another writer's rewrite");
    });
    assert!(
        matches!(outcome, Err(fsx::Unread::Failed(_))),
        "a look whose entry moved to another file while it was read fails"
    );
    assert!(!state.records.contains_key(VAULT), "nothing read is taken");
    assert_eq!(stored_file(&server).0, 2);
    look_at_records(&mut state, VAULT, &path, None, reading)
        .unwrap_or_else(|_| panic!("the entry read again"));
    assert!(state.records[VAULT].store.holds(vault.next.id()));
}

/// A server whose mark names format 2, on a build that does not read format 2, serves no
/// record and pulls none: such a build reads its file as the records before its first
/// version-2 one, and an answer of those would state a digest and a want of a log it does not
/// hold whole, which a format-2 puller would take for the server's newest records. Its GET
/// stating format 2 is answered a `500`, its peer pull asks nothing, and its file is
/// unchanged; on a build that reads format 2 the same server serves its records.
///
/// NEGATIVE CONTROL (2026-11-18): `unread_format` answering `None` and the pull's check
/// dropped (as before this part), run against this test and seen to fail with ``a marked
/// server that does not read its format serves nothing``, `left: 200` against `right: 500`.
#[test]
fn vault_checkpoint_a_marked_server_that_reads_no_format_two_serves_nothing() {
    let mut seat = Seat::found();
    let base = seat.held.clone();
    let n1 = seat.sign(false);
    let v2 = seat.sign(true);
    let marked = Server::holding(follows(), &base);
    assert_eq!(marked.post_two(&[&n1, &v2]).status, 204);
    assert_eq!(marked.format(), FORMAT_TWO);
    let served = marked.get(&two());
    assert_eq!(served.status, 200, "{}", served.head);
    let file = marked.file();

    marked.suffers(Faults::default());
    let refused = marked.get(&two());
    assert_eq!(
        refused.status, 500,
        "a marked server that does not read its format serves nothing: {}",
        refused.head
    );

    // Its pull of a format-2 peer asks nothing.
    let peer = Server::holding(follows(), &base);
    assert_eq!(peer.post_two(&[&n1, &v2]).status, 204);
    let asked = peer.running().records_gets();
    let _ = marked.running().pull_from_peers(&[peer.as_peer()]);
    assert_eq!(
        peer.running().records_gets(),
        asked,
        "a marked server that does not read its format pulls nothing"
    );
    assert_eq!(marked.file(), file);
}

/// The follow pull asks a peer that refused this server's pull at several endpoints once, and
/// an answer it judged whole proves the digest the peer stated, as a pull's does. A pass's pull
/// of a format-2 peer that holds nothing past this server's store is refused at each of the
/// peer's two endpoints; the pass names the peer once with both, the follow pull asks it at the
/// first, which answers with nothing to take, and the peer's stated digest is then proven at
/// that endpoint.
///
/// NEGATIVE CONTROLS (2026-11-18), each a change to the code run against this test and seen
/// to fail:
/// - `pull_endpoints` keeping each refused endpoint as its own `LaterPeer` (as before this
///   part): ``a peer refused at several endpoints is named once``, `left: 2` against
///   `right: 1`.
/// - `follow_later_peers` proving nothing on an answer that brought nothing (as before this
///   part): ``the follow pull proves the digest the peer stated``.
#[test]
fn vault_checkpoint_the_follow_pull_asks_a_peer_once_and_proves_its_digest() {
    let seat = Seat::found();
    let base = seat.held.clone();
    let peer = Server::holding(follows(), &base);
    peer.mark().store(FORMAT_TWO).expect("the mark");
    let digest = peer.store().heads_digest().expect("a digest");
    let at = peer.running().addr;
    let url = format!("http://{at}/v1/{VAULT}");
    let endpoints = [(at, url.clone()), (at, url)];

    let puller = Server::holding(follows(), &base);
    let shared = &puller.running().shared;
    let wants = wants();
    let asked = peer.running().records_gets();
    let pulled = pull_endpoints(&endpoints, HANG, Instant::now, |at, url| {
        pull_from_peer(shared, at, url, &wants)
    });
    assert_eq!(
        peer.running().records_gets() - asked,
        2,
        "refused at each endpoint"
    );
    assert_eq!(
        pulled.later.len(),
        1,
        "a peer refused at several endpoints is named once"
    );
    assert_eq!(pulled.later[0].endpoints, endpoints.to_vec());
    assert!(
        !shared.lock_gate().proved(VAULT, &digest, &[at], &[0; 32]),
        "nothing proven yet"
    );
    assert_eq!(follow_later_peers(shared, &pulled.later), 0);
    assert_eq!(peer.running().records_gets() - asked, 3, "asked again once");
    assert!(
        shared.lock_gate().proved(VAULT, &digest, &[at], &[0; 32]),
        "the follow pull proves the digest the peer stated"
    );
    assert_eq!(puller.format(), FORMAT_ONE);
}
