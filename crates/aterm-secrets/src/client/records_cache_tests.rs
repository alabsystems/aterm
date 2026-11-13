// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age::Identity,
    commons::binding::Request,
    envelope,
    membership_log::{Entrant, Record, encode_records},
};
use std::sync::atomic::Ordering;

const SIGNER: [u8; 32] = [61; 32];
const OTHER: [u8; 32] = [47; 32];

fn signed(seed: &[u8; 32], tag: u8) -> Vec<u8> {
    let identity = Identity::from_secret_bytes([tag ^ 0x55; 32]);
    let request = Request::create(
        [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
    )
    .unwrap();
    let record = Record::found(
        seed,
        vec![Entrant {
            request,
            node_key: [tag; 32],
            name: "cache-node".into(),
            label: format!("m{tag}"),
        }],
        1,
    )
    .unwrap();
    encode_records([&record])
}

fn base(seed: &[u8; 32]) -> Store {
    Store::dormant(&[envelope::signer_public(seed).unwrap()])
}

#[test]
fn an_observed_capacity_change_refuses_even_a_warm_cached_snapshot() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    fsx::write_atomic(&paths.records_file(), &signed(&SIGNER, 1), 0o600).unwrap();
    let small = RecordBudgets {
        weighed: 16,
        keyed: 128,
        held: 32,
    };
    let large = RecordBudgets { held: 64, ..small };
    small.within(|| {
        drop(crate::records_storage::write_path(&paths.records_file(), true).unwrap());
        let cache = Cache::new();
        assert!(
            cache
                .read(&paths, "team".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_some()
        );
        let changed = cache.read_then(&paths, "team".into(), base(&SIGNER), || {
            large.within(|| {
                drop(crate::records_storage::write_path(&paths.records_file(), false).unwrap());
            });
        });
        assert!(changed.is_err());
        assert!(cache.read(&paths, "team".into(), base(&SIGNER)).is_err());
    });
}

#[test]
fn equal_fresh_membership_bytes_need_no_repeated_digest() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let wire = signed(&SIGNER, 1);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let cache = Cache::new();
    for _ in 0..2 {
        assert_eq!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode(),
            wire
        );
    }
    assert_eq!(cache.hashes.load(Ordering::Relaxed), 1);
}

#[test]
fn a_retained_genesis_pin_cannot_reuse_an_unpinned_cached_root() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let wire = signed(&SIGNER, 1);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let cache = Cache::new();
    let unpinned = cache
        .read(&paths, "fixture".into(), Store::with_compiled(None))
        .unwrap();
    let network = *unpinned.log().unwrap().network();
    assert_ne!(network, [92; 32]);
    let wrong = cache
        .read(
            &paths,
            "fixture".into(),
            Store::with_compiled(Some([92; 32])),
        )
        .unwrap();
    assert!(
        wrong.log().is_none(),
        "same bytes do not bypass a different root pin"
    );
    let correct = cache
        .read(
            &paths,
            "fixture".into(),
            Store::with_compiled(Some(network)),
        )
        .unwrap();
    assert_eq!(correct.log().unwrap().network(), &network);
}

#[test]
fn cache_reloads_the_complete_history_within_its_store_budget() {
    use crate::crossing::fixture::{Batch, RAISED};

    let batch = Batch::get();
    let records = batch.whole();
    let wire = encode_records(&records);
    assert!(wire.len() <= MAX_INPUT_BYTES, "the slice recovery path");
    let large = wire.repeat(MAX_INPUT_BYTES / wire.len() + 1);
    assert!(large.len() > MAX_INPUT_BYTES, "the streaming recovery path");
    for input in [wire.clone(), large] {
        RAISED.within(|| {
            let home = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(home.path());
            fsx::ensure_private_dir(&paths.dir).unwrap();
            fsx::write_atomic(&paths.records_file(), &input, 0o600).unwrap();
            let cache = Cache::new();
            for _ in 0..2 {
                let store = cache
                    .read(&paths, "team".into(), Store::with_compiled(None))
                    .unwrap();
                assert_eq!(store.log().unwrap().held_count(), records.len());
                assert!(store.encode() == wire, "every signed record survives");
            }
            assert!(
                std::fs::read(paths.records_file()).unwrap() == input,
                "recovery leaves the complete persisted input unchanged"
            );
        });
    }
}

/// Entry charges stay within the cache budget, letting the oldest go first; an entry
/// past the whole budget is read and never kept, and each read still answers its own file.
/// Each entry is charged its store's footprint, which counts its one record and the one chunk
/// of index that record starts (the chunk's 63 words), the file's copy of the record, the
/// entry itself, and its vault name and both copies of its lineage keys.
/// NEGATIVE CONTROL: the stores read together keep more than the budget, which a cache bounded
/// by its count of entries (16 before) would keep; and each charge is pinned to that sum, which
/// a charge leaving out the store or its index misses.
#[test]
fn the_cache_keeps_its_entries_within_its_byte_budget() {
    const ONE_CHUNK: usize = 63 * size_of::<u64>();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let wires: Vec<Vec<u8>> = (1..=6).map(|tag| signed(&SIGNER, tag)).collect();
    let vault = || String::from("fixture");
    let meter = Cache::new();
    let mut costs = Vec::new();
    for wire in &wires {
        fsx::write_atomic(&paths.records_file(), wire, 0o600).unwrap();
        meter.read(&paths, vault(), base(&SIGNER)).unwrap();
        costs.push(meter.entries.lock().unwrap().back().unwrap().kept);
    }
    let lineage = base(&SIGNER).keys().map_or(0, |keys| {
        keys.to_vec().capacity() * 2 * size_of::<[u8; 32]>()
    });
    for (wire, &cost) in wires.iter().zip(&costs) {
        let footprint = base(&SIGNER).recovering(wire).footprint();
        assert_eq!(footprint, wire.len() + ONE_CHUNK, "the index counts");
        assert_eq!(
            cost,
            footprint + wire.len() + size_of::<Entry>() + vault().capacity() + lineage,
            "the charge is the store, the file, the entry and its context"
        );
    }
    let budget = costs[0] + costs[1] + costs[2];
    assert!(costs.iter().sum::<usize>() > budget);
    let cache = Cache::with_budget(budget);
    for (wire, cost) in wires.iter().zip(&costs) {
        fsx::write_atomic(&paths.records_file(), wire, 0o600).unwrap();
        let store = cache.read(&paths, vault(), base(&SIGNER)).unwrap();
        assert_eq!(store.encode(), *wire);
        let entries = cache.entries.lock().unwrap();
        let kept: usize = entries.iter().map(|entry| entry.kept).sum();
        assert!(kept <= budget, "{kept} bytes kept within {budget}");
        assert_eq!(entries.back().unwrap().kept, *cost);
    }
    assert!(cache.entries.lock().unwrap().len() < wires.len());

    let tiny = Cache::with_budget(costs[0] - 1);
    fsx::write_atomic(&paths.records_file(), &wires[0], 0o600).unwrap();
    for _ in 0..2 {
        let store = tiny.read(&paths, "fixture".into(), base(&SIGNER)).unwrap();
        assert_eq!(store.encode(), wires[0]);
        assert!(tiny.entries.lock().unwrap().is_empty());
    }
    assert_eq!(
        tiny.hashes.load(Ordering::Relaxed),
        2,
        "nothing kept, nothing reused"
    );
}

#[test]
fn empty_stores_still_spend_entry_and_context_budget() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    fsx::write_atomic(&paths.records_file(), b"", 0o600).unwrap();
    let budget = 4 * size_of::<Entry>();
    let cache = Cache::with_budget(budget);
    for i in 0..12 {
        let vault = format!("empty-{i}");
        let store = Store::for_vault(&vault).unwrap();
        assert!(
            cache
                .read(&paths, vault, store)
                .unwrap()
                .encode()
                .is_empty()
        );
    }
    {
        let entries = cache.entries.lock().unwrap();
        assert!(!entries.is_empty());
        assert!(
            entries.len() < 12,
            "empty files must still evict old contexts"
        );
        assert!(entries.iter().all(|entry| entry.kept >= size_of::<Entry>()));
        assert!(entries.iter().map(|entry| entry.kept).sum::<usize>() <= budget);
        assert!(!entries.iter().any(|entry| entry.vault == "empty-0"));
    }

    let mut large_name = String::with_capacity(budget + 1);
    large_name.push_str("empty-large-name");
    cache
        .read(
            &paths,
            large_name,
            Store::for_vault("empty-large-name").unwrap(),
        )
        .unwrap();
    let lineage: Vec<[u8; 32]> = (0..budget / size_of::<[u8; 32]>() + 1)
        .map(|i| {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&u64::try_from(i).unwrap().to_le_bytes());
            key
        })
        .collect();
    cache
        .read(
            &paths,
            "empty-large-lineage".into(),
            Store::dormant(&lineage),
        )
        .unwrap();
    let entries = cache.entries.lock().unwrap();
    assert!(
        !entries
            .iter()
            .any(|entry| entry.vault == "empty-large-name")
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.vault == "empty-large-lineage")
    );
}

#[test]
fn warmed_membership_still_reads_current_bytes_and_admitted_keys() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let first = signed(&SIGNER, 1);
    let second = signed(&OTHER, 2);
    fsx::write_atomic(&paths.records_file(), &first, 0o600).unwrap();
    let cache = Cache::new();
    assert!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .log()
            .is_some()
    );
    assert!(
        cache
            .read(&paths, "fixture".into(), base(&OTHER))
            .unwrap()
            .log()
            .is_none()
    );
    fsx::write_atomic(&paths.records_file(), &second, 0o600).unwrap();
    assert!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .log()
            .is_none()
    );
    assert_eq!(
        cache
            .read(&paths, "fixture".into(), base(&OTHER))
            .unwrap()
            .encode(),
        second
    );
    fsx::write_atomic(&paths.records_file(), b"malformed records", 0o600).unwrap();
    assert!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .log()
            .is_none()
    );
    fsx::write_atomic(&paths.records_file(), &first, 0o600).unwrap();
    assert_eq!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .encode(),
        first
    );
    std::fs::rename(paths.records_file(), paths.dir.join("held-records")).unwrap();
    assert!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .log()
            .is_none()
    );
}

/// A read of the records file as it was last read takes the kept store without reading it:
/// a large file is streamed and hashed once to observe it and once to decode it, and a small
/// one hashed once, then neither is read again. The same bytes renamed over it are a new
/// file, read again and taken from the entry they match, under whose stamp the next read is
/// not; a rewrite of the same length in place is a new stamp too (its times), and is read.
/// NEGATIVE CONTROL: with the stamp test in `read_then`'s first lookup forced true, no file
/// is read again once one was: this test fails at the renamed file's count, and
/// `warmed_membership_still_reads_current_bytes_and_admitted_keys` and
/// `the_cache_keeps_its_entries_within_its_byte_budget` get a store from before a write.
#[test]
fn an_unchanged_records_file_is_taken_by_its_stamp_without_reading_it() {
    for large in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let mut wire = signed(&SIGNER, 1);
        if large {
            wire.resize(MAX_INPUT_BYTES + 64, 0);
        }
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        let cache = Cache::new();
        let counts = || {
            (
                cache.observations.load(Ordering::Relaxed),
                cache.hashes.load(Ordering::Relaxed),
            )
        };
        let read = || {
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode()
        };
        let cold = if large { (1, 2) } else { (1, 1) };
        for _ in 0..3 {
            assert_eq!(read(), signed(&SIGNER, 1));
            assert_eq!(counts(), cold, "large={large}");
        }

        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        let renamed = (cold.0 + 1, cold.1 + usize::from(large));
        for _ in 0..2 {
            assert_eq!(read(), signed(&SIGNER, 1));
            assert_eq!(counts(), renamed, "large={large}");
        }

        let mut other = signed(&OTHER, 2);
        other.resize(wire.len(), 0);
        let before = fsx::stamp(&paths.records_file()).unwrap();
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(paths.records_file())
                .unwrap();
            file.write_all(&other).unwrap();
            file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(1))
                .unwrap();
        }
        let after = fsx::stamp(&paths.records_file()).unwrap();
        assert_eq!((after.ino, after.len), (before.ino, before.len));
        assert_ne!(after, before);
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_none(),
            "large={large}: the rewritten file holds no record under SIGNER"
        );
        assert_eq!(counts().0, renamed.0 + 1, "large={large}");
    }
}

/// Two vault folders whose records files hold the same bytes, as two Macs of a test's lab do,
/// read in turn: the second is read once and taken from the first's entry, and from then on
/// each is taken by its own stamp, unread. NEGATIVE CONTROL: an entry known by one file
/// (`STAMPS` = 1) reads each file again at every turn, and hashes it when large.
#[test]
fn two_files_of_the_same_records_are_each_taken_by_their_stamp() {
    for large in [false, true] {
        let homes = [
            aterm_tempfile::tempdir().unwrap(),
            aterm_tempfile::tempdir().unwrap(),
        ];
        let paths = homes.each_ref().map(|home| Paths::for_home(home.path()));
        let mut wire = signed(&SIGNER, 1);
        if large {
            wire.resize(MAX_INPUT_BYTES + 64, 0);
        }
        for paths in &paths {
            fsx::ensure_private_dir(&paths.dir).unwrap();
            fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        }
        let cache = Cache::new();
        let read = |paths: &Paths| {
            cache
                .read(paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode()
        };
        for paths in &paths {
            assert_eq!(read(paths), signed(&SIGNER, 1));
        }
        let observed = cache.observations.load(Ordering::Relaxed);
        assert_eq!(observed, 2, "large={large}: each file is read once");
        for _ in 0..3 {
            for paths in &paths {
                assert_eq!(read(paths), signed(&SIGNER, 1));
            }
        }
        assert_eq!(
            cache.observations.load(Ordering::Relaxed),
            observed,
            "large={large}: each file is taken by its stamp"
        );
        assert_eq!(cache.entries.lock().unwrap().len(), 1, "large={large}");
    }
}

#[test]
fn changed_files_cannot_return_warm_small_or_large_records() {
    for large in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let mut wire = signed(&SIGNER, 1);
        if large {
            wire.resize(MAX_INPUT_BYTES + 64, 0);
        }
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        let cache = Cache::new();
        let expected = cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .encode();
        let prior_digest = cache.entries.lock().unwrap()[0].digest;
        let changed = cache.read_then(&paths, "fixture".into(), base(&SIGNER), || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(paths.records_file())
                .unwrap()
                .set_len(0)
                .unwrap();
        });
        assert!(changed.is_err());
        {
            let entries = cache.entries.lock().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].digest, prior_digest);
        }
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        assert_eq!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode(),
            expected
        );
    }
}

/// A writer renames a new records file over the one a read holds open, as every writer of the
/// client's records does (`write_atomic`): the read returns the store of the bytes it read,
/// from the file it opened, cold (a small and a large observation) or from a warm entry taken
/// by its stamp, and the next read the new file's. The rename moves the replaced inode's
/// change time, which a read once refused as a change, failing a node-pass test's poll
/// (2026-11-06). NEGATIVE CONTROL: [`a_held_file_changed_another_way_is_still_refused`].
#[test]
fn a_file_renamed_over_mid_read_returns_what_the_read_held() {
    for (large, warm) in [(false, false), (true, false), (false, true)] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let padded = |mut wire: Vec<u8>| {
            if large {
                wire.resize(MAX_INPUT_BYTES + 64, 0);
            }
            wire
        };
        let (before, after) = (padded(signed(&SIGNER, 1)), padded(signed(&SIGNER, 2)));
        let read = |cache: &Cache| {
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode()
        };
        fsx::write_atomic(&paths.records_file(), &after, 0o600).unwrap();
        let read_after = read(&Cache::new());
        fsx::write_atomic(&paths.records_file(), &before, 0o600).unwrap();
        let read_before = read(&Cache::new());
        assert_ne!(read_before, read_after);
        let cache = Cache::new();
        if warm {
            assert_eq!(read(&cache), read_before);
        }
        let observed = cache.observations.load(Ordering::Relaxed);
        let held = cache.read_then(&paths, "fixture".into(), base(&SIGNER), || {
            fsx::write_atomic(&paths.records_file(), &after, 0o600).unwrap();
        });
        let case = format!("large={large} warm={warm}");
        assert_eq!(held.expect(&case).encode(), read_before, "{case}");
        assert_eq!(
            cache.observations.load(Ordering::Relaxed) - observed,
            usize::from(!warm),
            "{case}: a warm read takes its entry by its stamp, a cold one streams the file"
        );
        assert_eq!(read(&cache), read_after, "{case}");
    }
}

/// A file a read holds open that changed in any other way is still refused: written in place
/// and then renamed over (its last link gone, its length changed too), or renamed aside (still
/// linked, only its change time moved, as measured on APFS).
#[test]
fn a_held_file_changed_another_way_is_still_refused() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let (records, aside) = (paths.records_file(), paths.dir.join("records.aside"));
    let wire = signed(&SIGNER, 1);
    let in_place_then_over = || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&records)
            .unwrap()
            .set_len(0)
            .unwrap();
        fsx::write_atomic(&records, &wire, 0o600).unwrap();
    };
    let renamed_aside = || std::fs::rename(&records, &aside).unwrap();
    let changes: [(&str, &dyn Fn()); 2] = [
        ("written in place, then renamed over", &in_place_then_over),
        ("renamed aside", &renamed_aside),
    ];
    for (name, change) in changes {
        fsx::write_atomic(&records, &wire, 0o600).unwrap();
        let changed = Cache::new().read_then(&paths, "fixture".into(), base(&SIGNER), change);
        assert!(changed.is_err(), "{name}");
    }
}

#[test]
fn changed_large_decode_never_installs_its_partial_store() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let mut wire = signed(&SIGNER, 1);
    wire.resize(MAX_INPUT_BYTES + 64, 0);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let cache = Cache::new();
    let changed = cache.read_then(&paths, "fixture".into(), base(&SIGNER), || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(paths.records_file())
            .unwrap()
            .set_len(0)
            .unwrap();
    });
    assert!(changed.is_err());
    assert!(cache.entries.lock().unwrap().is_empty());
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    assert!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .log()
            .is_some()
    );
}

#[test]
fn a_failed_observation_preserves_the_previous_cache_and_can_recover() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let wire = signed(&SIGNER, 1);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let cache = Cache::new();
    cache.read(&paths, "fixture".into(), base(&SIGNER)).unwrap();
    let digest = cache.entries.lock().unwrap()[0].digest;
    fsx::fail_reads(&paths.records_file(), 1);
    assert!(cache.read(&paths, "fixture".into(), base(&SIGNER)).is_err());
    assert_eq!(cache.entries.lock().unwrap()[0].digest, digest);
    assert_eq!(
        cache
            .read(&paths, "fixture".into(), base(&SIGNER))
            .unwrap()
            .encode(),
        wire
    );
}

#[test]
fn the_decoding_digest_includes_the_entire_ignored_tail() {
    let mut wire = signed(&SIGNER, 1);
    wire.resize(MAX_INPUT_BYTES + 64, 0);
    let mut reader = Hashed::new(std::io::Cursor::new(&wire));
    let recovered = base(&SIGNER).recovering_read(&mut reader).unwrap();
    assert!(recovered.log().is_some());
    assert_eq!(reader.finish(), sha256(&wire));
}

#[test]
fn reversion_after_decoder_reads_cannot_authenticate_different_bytes() {
    use std::os::unix::fs::FileExt;

    struct Reverting<'a> {
        file: std::fs::File,
        original: &'a [u8],
        replacement: &'a [u8],
        read: usize,
        changed: bool,
        restored: bool,
    }
    impl Read for Reverting<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if !self.changed {
                self.file.write_all_at(self.replacement, 0)?;
                self.changed = true;
            }
            let n = self.file.read(out)?;
            self.read += n;
            if self.read == self.original.len() && !self.restored {
                self.file.write_all_at(self.original, 0)?;
                self.restored = true;
            }
            Ok(n)
        }
    }

    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join("records");
    let mut original = signed(&SIGNER, 1);
    let mut replacement = signed(&SIGNER, 2);
    original.resize(MAX_INPUT_BYTES + 64, 0);
    replacement.resize(original.len(), 0);
    assert!(base(&SIGNER).recovering(&replacement).log().is_some());
    fsx::write_atomic(&path, &original, 0o600).unwrap();
    let mut reader = Reverting {
        file: std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap(),
        original: &original,
        replacement: &replacement,
        read: 0,
        changed: false,
        restored: false,
    };
    let outcome = recover_observation(base(&SIGNER), &mut reader, sha256(&original));
    assert!(reader.changed && reader.restored);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(outcome.is_err());
}

/// A read that brackets a writer's append (the file it holds open grew) fails its check, as
/// any read of a file changed in place does, and is started again on the file as it then is:
/// the reader gets the store with the appended record, cold or from a warm entry, where one
/// read alone refuses. A file that moves under every try still fails the read, after
/// [`MOVED_READS`] more tries.
/// NEGATIVE CONTROL (2026-11-11): with [`Cache::read_moving`] returning its first read's
/// answer, the bracketing read fails (`read again after the append`).
#[test]
fn a_read_that_brackets_an_append_reads_the_file_again() {
    use crate::membership_log::store::classified_tests::chain;
    use std::io::Write as _;

    let c = chain();
    let empty = || Store::for_vault("").unwrap();
    let append = |paths: &Paths, bytes: &[u8]| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(paths.records_file())
            .unwrap()
            .write_all(bytes)
            .unwrap();
    };
    for warm in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        fsx::write_atomic(&paths.records_file(), c.genesis.bytes(), 0o600).unwrap();
        let cache = Cache::new();
        if warm {
            cache.read(&paths, String::new(), empty()).unwrap();
        }
        let alone = cache.read_then(&paths, String::new(), empty(), || {
            append(&paths, c.first_by_m1.bytes());
        });
        assert!(alone.is_err(), "warm={warm}: one read refuses the append");

        let tries = std::cell::Cell::new(0);
        let read = cache.read_moving(&paths, String::new(), empty(), |attempt| {
            tries.set(tries.get() + 1);
            if attempt == 0 {
                append(&paths, c.admit.bytes());
            }
        });
        let read = read.expect("read again after the append");
        assert_eq!(tries.get(), 2, "warm={warm}");
        assert_eq!(
            read.encode(),
            encode_records([&c.genesis, &c.first_by_m1, &c.admit]),
            "warm={warm}"
        );

        tries.set(0);
        let moving = cache.read_moving(&paths, String::new(), empty(), |_| {
            tries.set(tries.get() + 1);
            append(&paths, b"\0");
        });
        assert!(moving.is_err(), "warm={warm}: it never stood still");
        assert_eq!(tries.get(), MOVED_READS + 1, "warm={warm}");
    }
}
