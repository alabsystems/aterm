// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A rooted store's rewritten records file (tracker FL-148D): the store read back from the file
//! a rewrite put in place is the store that was written, a record appended since is read with
//! it, a frame that a root's body does not bind is refused whole, and the file's head reads the
//! same log.

use super::*;
use crate::crossing::FORMAT_TWO;
#[cfg(unix)]
use crate::fsx;
use crate::membership_log::checkpoint::tests::{
    A, B, C, HOUR, NOW, SPLIT, T, draft, hold, name, one, share_at, two,
};
use crate::membership_log::checkpoint::{DAY_MS, MIN_AGE_MS, plan_at, propose};
use crate::membership_log::rooted::tests::{
    carry_of_dropped, common, random_vault, same_log, side_b, sleeper, store_of,
};
use crate::membership_log::served::tests::holding;
use crate::membership_log::store::REPLY_CAP;
use crate::membership_log::{Inserted, Statement, budgets, encode_records};
#[cfg(unix)]
use crate::records_storage::rewrite::{self, Predecessor, What};

const TWO: Reading = Reading::Format(FORMAT_TWO);

/// An empty store that reads format 2, as the node of a rewritten file does.
fn empty() -> Store {
    Store::with_compiled(None).read_at(TWO)
}

/// The sleeper's vault ([`sleeper`]) pruned at its cut: one root, one stub of a version-1 `Put`,
/// and C's record after the cut, which follows the dropped `Put` alone.
fn pruned_once() -> Log {
    let slept = sleeper();
    slept.l2.rooted_at(&slept.body).unwrap()
}

/// [`carry_of_dropped`]'s vault pruned at its first checkpoint and then at the next, whose
/// whole log keeps an op the first dropped: the first root is kept beside the next
/// ([`Rooting::earlier`]), and its stubs are of version-2 records.
fn pruned_twice() -> Log {
    let (mut whole, plan, _, _) = carry_of_dropped(false);
    let first = plan.body;
    for (at, mac) in [(2, A), (3, B), (4, C)] {
        let record = one(draft(&whole, mac, T + at * DAY_MS, name("after")), mac);
        hold(&mut whole, record);
    }
    let shared = T + MIN_AGE_MS + HOUR;
    share_at(&mut whole, A, &first, shared);
    share_at(&mut whole, B, &first, shared);
    let now = T + 5 * DAY_MS + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
    let next = propose(&whole, now).expect("a next proposal");
    for mac in [A, B, C] {
        share_at(&mut whole, mac, &next.body, now + HOUR);
    }
    whole.adopt(&first).unwrap().adopt(&next.body).unwrap()
}

/// A Mac pruned at A's side of a split that took B's side's root from a peer: two roots, a
/// stub of each side's, and B's record that follows B's dropped one, which follows A's.
fn two_sides() -> Log {
    let common = common();
    let put = &common.put;
    let mut side_a = common.log.clone();
    let a1 = two(
        put(&side_a, A, SPLIT - DAY_MS, "a.key", b"a1"),
        A,
        Some(&Statement {
            supersedes: vec![*common.a0.id()],
            carries: None,
        }),
    );
    let a1 = hold(&mut side_a, a1);
    let cut_a = plan_at(&side_a, SPLIT, &[*a1.id()]).unwrap();
    share_at(&mut side_a, A, &cut_a.body, SPLIT + 2 * DAY_MS);
    let side = side_b(&common);
    let mut side_log = side.log.clone();
    share_at(&mut side_log, C, &side.body, SPLIT + 3 * DAY_MS);
    let peer = store_of(side_log.adopt(&side.body).unwrap());
    let mut receiver = store_of(side_a.adopt(&cut_a.body).unwrap());
    let missing = peer.missing_served(&receiver.have(), REPLY_CAP);
    receiver
        .ingest_served(missing.records, missing.served.as_ref())
        .unwrap();
    receiver.log().unwrap().clone()
}

/// Seeded vaults ([`random_vault`]) pruned at a first final checkpoint and, where a later one
/// is final too, at that: each log a Mac that pruned as the checkpoints came holds.
fn random_pruned(seeds: std::ops::RangeInclusive<u64>) -> Vec<(String, Log)> {
    let mut out = Vec::new();
    for seed in seeds {
        let mut world = random_vault(seed, 100, 30);
        let Some(first) = propose(&world.whole(), NOW) else {
            continue;
        };
        world.settle(&first.body);
        let Ok(once) = world.whole().adopt(&first.body) else {
            continue;
        };
        out.push((format!("seed {seed} pruned once"), once.clone()));
        world.run(seed, 120);
        let later = world.clock + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
        let Some(next) = propose(&world.whole(), later) else {
            continue;
        };
        let Body::Checkpoint { cut_ms, .. } = &next.body else {
            panic!("a checkpoint");
        };
        world.clock = world.clock.max(cut_ms + HOUR);
        world.settle(&next.body);
        let mut twice = once;
        twice.merge(&world.whole()).unwrap();
        if let Ok(twice) = twice.adopt(&next.body) {
            out.push((format!("seed {seed} pruned twice"), twice));
        }
    }
    out
}

/// The length a rewritten file's writer holds to the format's capacity
/// ([`Store::stored_len`], `client::persist_stored`) is the length of the file it writes
/// ([`Store::write_stored`]): for a log pruned once, one pruned twice, one that took another
/// side's root, a whole log (no frame), and a rooted store that also holds a record waiting
/// on a parent. A rooted log's stubs are in its frame and counted once.
///
/// NEGATIVE CONTROL, a change to the code run against this test and seen to fail:
/// [`Store::stored_len`] counting the frame beside the log's own byte bound
/// ([`Store::encoded_len`], which counts the stubs, as `client::persist_stored` did): ``pruned
/// once: the bound is the file's length``, `left: 4989` against `right: 4787`.
#[test]
fn vault_checkpoint_a_rewritten_files_bound_is_the_length_its_writer_writes() {
    let slept = sleeper();
    let mut waits = store_of(pruned_once());
    let lone = one(draft(&slept.l2, A, NOW, name("lone")), A);
    let after = one(
        draft(
            &{
                let mut ahead = slept.l2.clone();
                hold(&mut ahead, lone);
                ahead
            },
            A,
            NOW + 1,
            name("after"),
        ),
        A,
    );
    waits.ingest_records(vec![after]).unwrap();
    assert_eq!(
        waits.inventory().len(),
        waits.complete_inventory().len() + 1
    );
    for (what, store) in [
        ("pruned once", store_of(pruned_once())),
        ("pruned twice", store_of(pruned_twice())),
        ("two sides", store_of(two_sides())),
        ("a whole log", store_of(slept.l2.clone())),
        ("a record waiting", waits),
    ] {
        let mut file = Vec::new();
        store.write_stored(&mut file).unwrap();
        assert_eq!(
            store.stored_len().unwrap(),
            file.len(),
            "{what}: the bound is the file's length"
        );
    }
}

/// A records entry in a scratch folder whose data a rewrite replaced with `log`'s store, as
/// [`Store::write_stored`] writes it: the folder, the entry and the data file's bytes.
#[cfg(unix)]
fn rewritten(log: &Log) -> (aterm_tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
    let store = store_of(log.clone());
    let dir = aterm_tempfile::tempdir().unwrap();
    let entry = dir.path().join("records");
    // What the entry held before is the records the prune started from; here, its own.
    let before = encode_records(log.records());
    fsx::write_atomic(&entry, &before, 0o600).unwrap();
    let root = log
        .rooting()
        .and_then(|rooting| rooting.roots.last())
        .map_or([0; 32], |root| root.id);
    let done = rewrite::rewrite(
        &entry,
        Predecessor::of(&before),
        What::Checkpoint(root),
        &|out| store.write_stored(out),
    )
    .unwrap();
    let custody = rewrite::data_path(&entry).unwrap();
    assert_eq!((custody.generation, &custody.path), (1, &done.path));
    let bytes = std::fs::read(&custody.path).unwrap();
    assert_eq!(bytes.len(), done.len);
    (dir, entry, bytes)
}

/// The bytes of the records file `log`'s store is rewritten to: through a rewrite of an entry
/// ([`rewritten`]) where a records entry can be rewritten.
fn stored_file(log: &Log) -> Vec<u8> {
    #[cfg(unix)]
    {
        rewritten(log).2
    }
    #[cfg(not(unix))]
    {
        let mut bytes = Vec::new();
        store_of(log.clone()).write_stored(&mut bytes).unwrap();
        bytes
    }
}

/// Whether `read` is the log `written`: [`same_log`] (records and the order over them, stubs,
/// roots, counts, evaluation), what a peer is told of it ([`holding`]: the earlier roots and
/// the heads digest too), its records in their order, and the bytes its bound counts.
fn is_the_log(written: &Log, read: &Log) -> bool {
    let order = |log: &Log| log.records().map(|record| *record.id()).collect::<Vec<_>>();
    same_log(written, read)
        && holding(written) == holding(read)
        && order(written) == order(read)
        && written.total_bytes() == read.total_bytes()
        && written.waiting() == read.waiting()
}

/// A rooted store round-trips through the file a rewrite puts in place: the store read back
/// from the rewritten entry's data file is the store that was written, for a log pruned once
/// (a version-1 stub), pruned twice (an earlier root kept beside the newest, version-2 stubs),
/// holding both sides' roots of a split, and for seeded vaults pruned once and twice. A record
/// appended to the file since, and a frame of one cut short after it, read back as the log that
/// then took the record. A whole log's file is its records' stream, as it always was, and reads
/// as it always did.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::FrameSkipped` (a read passes over the frame and
/// takes the records after it as a whole log's), run from the test's first line and seen to
/// fail with ``pruned once`` at the assertion that the log read is the log written: the record
/// that follows the dropped one waits, and the log read has no root.
#[test]
fn vault_checkpoint_a_rooted_store_round_trips_through_its_rewritten_file() {
    let mut logs = vec![
        ("pruned once".to_owned(), pruned_once()),
        ("pruned twice".to_owned(), pruned_twice()),
        ("two sides of a split".to_owned(), two_sides()),
    ];
    logs.extend(random_pruned(1..=8));
    let (mut earlier, mut two_roots, mut twice) = (0, 0, 0);
    for (what, log) in &logs {
        let rooting = log.rooting().unwrap_or_else(|| panic!("{what}: rooted"));
        earlier += usize::from(!rooting.earlier.is_empty());
        two_roots += usize::from(rooting.roots.len() > 1);
        twice += usize::from(what.ends_with("twice"));
        let bytes = stored_file(log);
        assert!(bytes.starts_with(KEPT_MAGIC), "{what}");
        let read = empty().recovering_stored(&bytes).unwrap();
        assert!(is_the_log(log, read.log().unwrap()), "{what}");
        // Written again, the file is the same bytes.
        let mut again = Vec::new();
        read.write_stored(&mut again).unwrap();
        assert_eq!(again, bytes, "{what}");
        // A reader of format 1 is never handed the file (its pin is refused); given its bytes,
        // it takes no record of them as a rooted log's.
        assert!(
            crate::membership_log::decode_records(&bytes).is_err(),
            "{what}"
        );
    }
    assert!(logs.len() >= 8, "{} logs", logs.len());
    assert!(
        earlier >= 1 && two_roots >= 1 && twice >= 2,
        "{earlier} {two_roots} {twice}"
    );

    // A record appended since the rewrite, then a frame cut short.
    let log = pruned_once();
    let mut bytes = stored_file(&log);
    let later = one(draft(&log, A, NOW + DAY_MS, name("later")), A);
    let mut with = log.clone();
    assert_eq!(with.insert(later.clone()).unwrap(), Inserted::Held);
    let wire = later.wire_bytes().unwrap();
    bytes.extend_from_slice(&wire);
    let read = empty().recovering_stored(&bytes).unwrap();
    assert!(is_the_log(&with, read.log().unwrap()));
    bytes.extend_from_slice(&wire[..wire.len() / 2]);
    let read = empty().recovering_stored(&bytes).unwrap();
    assert!(is_the_log(&with, read.log().unwrap()));

    // A whole log.
    let whole = sleeper().l2;
    let store = store_of(whole.clone());
    assert!(store.kept_frame().unwrap().is_none());
    let mut bytes = Vec::new();
    store.write_stored(&mut bytes).unwrap();
    assert_eq!(bytes, encode_records(whole.records()));
    let read = empty().recovering_stored(&bytes).unwrap();
    assert!(read.log().unwrap().rooting().is_none());
    assert_eq!(
        holding(read.log().unwrap()),
        holding(empty().recovering(&bytes).log().unwrap())
    );
    assert_eq!(holding(read.log().unwrap()), holding(&whole));
}

/// The file `bytes` with its frame changed by `forge`.
fn forged(bytes: &[u8], forge: impl FnOnce(&mut Kept)) -> Vec<u8> {
    let (mut kept, lead) = Kept::decode(bytes, TWO).unwrap();
    forge(&mut kept);
    let mut out = kept.encode();
    out.extend_from_slice(&bytes[lead..]);
    out
}

/// What reading the file `bytes` back is refused with; `None` where it is read.
fn refusal(bytes: &[u8]) -> Option<String> {
    empty()
        .recovering_stored(bytes)
        .err()
        .map(|error| error.to_string())
}

/// A frame is read only as its roots' bodies bind it: a stub with one field changed (the
/// dropped `Put`'s clock) is refused, since its root's `dropped` digest hashes each stub's
/// fields, and so is a stub of a record no kept root dropped, a root's dropped id with no stub,
/// and a frame with no root. Each is refused whole, with no record of the file taken, and a
/// store never writes such a frame.
///
/// NEGATIVE CONTROLS (2026-11-17), each run from the test's first line and seen to fail with
/// ``assertion `left == right` failed``, `left: None`: `Mutant::Unbound` (a root's digest is
/// not held to the stubs kept for it) against `right: Some("a kept root's digest does not name
/// the stubs kept for it")`, the forged clock read; `Mutant::Uncovered` (a stub no kept root
/// dropped is read too) against `right: Some("a kept stub no kept root dropped")`, the made-up
/// stub read. Both are run again at the test's end, where each reads its forged file.
#[test]
fn vault_checkpoint_a_kept_frame_no_root_binds_is_refused() {
    let log = pruned_once();
    let mut bytes = Vec::new();
    store_of(log.clone()).write_stored(&mut bytes).unwrap();
    assert_eq!(refusal(&bytes), None, "the file as written is read");

    let clock = forged(&bytes, |kept| {
        for stub in kept.rooting.stubs.values_mut() {
            stub.authored_ms += HOUR;
        }
    });
    assert_eq!(
        refusal(&clock).as_deref(),
        Some("a kept root's digest does not name the stubs kept for it")
    );
    let extra = forged(&bytes, |kept| {
        let mut made_up = kept.rooting.stubs.values().next().unwrap().clone();
        made_up.parents = vec![*made_up.parents.first().unwrap()];
        kept.rooting.stubs.insert([0xEE; 32], made_up);
    });
    assert_eq!(
        refusal(&extra).as_deref(),
        Some("a kept stub no kept root dropped")
    );
    let none = forged(&bytes, |kept| kept.rooting.stubs.clear());
    assert_eq!(
        refusal(&none).as_deref(),
        Some("a kept root dropped a record no stub stands for")
    );
    let rootless = forged(&bytes, |kept| kept.rooting.roots.clear());
    assert_eq!(
        refusal(&rootless).as_deref(),
        Some("a rooted log keeps a root")
    );

    // A store whose rooting fails the laws writes no file.
    let (mut kept, _) = Kept::decode(&bytes, TWO).unwrap();
    for stub in kept.rooting.stubs.values_mut() {
        stub.authored_ms += HOUR;
    }
    let unbound = log.rebuilt(kept.into_rooting(), &BTreeSet::new()).unwrap();
    let store = store_of(unbound);
    assert!(store.kept_frame().is_err());
    assert!(store.write_stored(&mut Vec::new()).is_err());

    MUTANT.set(Some(Mutant::Unbound));
    let read = refusal(&clock);
    MUTANT.set(None);
    assert_eq!(read, None, "Unbound reads a stub its root does not bind");
    MUTANT.set(Some(Mutant::Uncovered));
    let read = refusal(&extra);
    MUTANT.set(None);
    assert_eq!(read, None, "Uncovered reads a stub no root dropped");
}

/// A frame reads exactly or not at all: one cut short, one with a byte its fields do not
/// count, one of another network than the genesis after it, one with no genesis after it, and
/// one read into a store that holds records are each refused; and a reader of format 1
/// refuses a frame that holds a version-2 record's stub, as it refuses the record.
#[test]
fn vault_checkpoint_a_kept_frame_reads_exactly() {
    let log = pruned_twice();
    let mut bytes = Vec::new();
    store_of(log.clone()).write_stored(&mut bytes).unwrap();
    let (kept, lead) = Kept::decode(&bytes, TWO).unwrap();
    assert_eq!(kept.encode(), bytes[..lead]);
    assert_eq!(refusal(&bytes), None);
    assert!(
        log.rooting()
            .unwrap()
            .stubs
            .values()
            .any(|stub| stub.frame.is_some()),
        "a version-2 record's stub"
    );
    assert!(Kept::decode(&bytes, Reading::ONE).is_err());
    assert!(
        Store::with_compiled(None)
            .recovering_stored(&bytes)
            .is_err(),
        "a store that reads format 1"
    );

    for cut in [3, 4, 11, 12, 44, lead / 2, lead - 1] {
        assert!(Kept::decode(&bytes[..cut], TWO).is_err(), "cut at {cut}");
    }
    assert!(refusal(&bytes[..lead - 1]).is_some());
    // One byte more inside the frame's length.
    let mut longer = bytes[..lead].to_vec();
    longer.push(0);
    let len = (lead - 12 + 1) as u64;
    longer[4..12].copy_from_slice(&len.to_be_bytes());
    assert_eq!(
        Kept::decode(&longer, TWO).unwrap_err().to_string(),
        "a kept frame has bytes its fields do not count"
    );
    let other = forged(&bytes, |kept| kept.network = [0x11; 32]);
    assert!(refusal(&other).is_some(), "a frame of another network");
    assert_eq!(
        refusal(&bytes[..lead]).as_deref(),
        Some("a rooted records file holds its genesis after its frame")
    );
    let held = store_of(sleeper().l1).read_at(TWO);
    assert!(
        held.recovering_stored(&bytes).is_err(),
        "a store that holds records"
    );
}

/// The head of a rooted log's rewritten file reads the log a full read of the file reads: it
/// keeps the frame the file begins with, so a record that follows a dropped one is placed by
/// its stub, and each record stands where the file holds it, after the frame.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::HeadWithoutFrame` (a rooted log's head holds its
/// records alone), run from the test's first line and seen to fail with ``the head of a rooted
/// log's file is read`` (the `expect` of the open through the head): the head's records do not
/// cover the file, and would not place. It is run again at the test's end.
#[cfg(unix)]
#[test]
fn vault_checkpoint_a_rewritten_file_is_read_through_its_head() {
    use crate::membership_log::head;
    let identity = A.identity();
    let context = head::context(&empty(), "");
    for log in [pruned_once(), pruned_twice()] {
        let (_dir, entry, bytes) = rewritten(&log);
        let data = rewrite::data_path(&entry).unwrap().path;
        let head_file = crate::records_storage::head_path(&entry).unwrap();
        assert!(!head_file.exists(), "a rewrite leaves no head");
        let full = || {
            let mut store = empty().recovering_stored(&bytes).unwrap();
            store.read_from(head::Origin::new(
                &head_file,
                fsx::stamp(&data).unwrap(),
                head::digest(&bytes),
                context,
                identity.recipient().0,
                std::time::SystemTime::now() + 2 * fsx::SETTLED,
            ));
            store
        };
        let through_head = || {
            let file = std::fs::File::open(&data).unwrap();
            let stamp = fsx::FileStamp::of(&fsx::meta_of(&file).unwrap());
            head::open(
                &head_file,
                &file,
                stamp,
                context,
                identity,
                empty(),
                head::Publication::Allowed,
            )
            .map(|(store, _)| store)
        };
        let store = full();
        let writes = head::WRITES.get();
        head::write(store.log().unwrap(), identity, &head::Resolved::default());
        assert_eq!(head::WRITES.get(), writes + 1, "the head is written");
        let read = through_head().expect("the head of a rooted log's file is read");
        assert!(is_the_log(&log, read.log().unwrap()));
        assert!(is_the_log(store.log().unwrap(), read.log().unwrap()));

        std::fs::remove_file(&head_file).unwrap();
        assert!(through_head().is_none(), "no head, no open through it");
        let store = full();
        MUTANT.set(Some(Mutant::HeadWithoutFrame));
        head::write(store.log().unwrap(), identity, &head::Resolved::default());
        MUTANT.set(None);
        assert!(head_file.exists(), "the mutant's head is written");
        assert!(
            through_head().is_none(),
            "HeadWithoutFrame's head is not the file"
        );
    }
}
