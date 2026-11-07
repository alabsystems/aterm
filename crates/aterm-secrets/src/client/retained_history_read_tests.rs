// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Receipt-reader measurements. These fixtures isolate journal replay from signed-log work.

use super::*;

struct Receipts {
    _dir: aterm_tempfile::TempDir,
    paths: Paths,
    journal: aterm_journal::Log,
    frontier: Frontier,
}

impl Receipts {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(dir.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let mut journal =
            aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, false, limits()).unwrap();
        journal.finish_open().unwrap();
        aterm_journal::write_private_durable(&paths.dir, "records.frontiers", PENDING).unwrap();
        Self {
            _dir: dir,
            paths,
            journal,
            frontier: Frontier::empty([1; 32]),
        }
    }

    fn append(&mut self, n: u64) {
        let mut id = [3; 32];
        id[..8].copy_from_slice(&n.to_le_bytes());
        let retired = self.frontier.heads();
        let mut encoded = b"VFD1".to_vec();
        encoded.extend_from_slice(&self.frontier.digest());
        encoded.extend_from_slice(&1u32.to_le_bytes());
        encoded.extend_from_slice(&(retired.len() as u32).to_le_bytes());
        encoded.extend_from_slice(&id);
        for id in retired {
            encoded.extend_from_slice(&id);
        }
        self.frontier
            .apply(&Delta::decode(&encoded).unwrap())
            .unwrap();
        let mut receipt = Vec::new();
        receipt.extend_from_slice(&[1; 32]);
        receipt.extend_from_slice(&[2; 32]);
        receipt.extend_from_slice(&1u16.to_le_bytes());
        receipt.extend_from_slice(&u16::from(n == 1).to_le_bytes());
        receipt.push(b'n');
        if n == 1 {
            receipt.extend_from_slice(&[2; 32]);
        }
        receipt.extend_from_slice(&encoded);
        self.journal.append(SUBJECT, receipt).unwrap();
        aterm_journal::write_private_durable(&self.paths.dir, "records.frontiers", READY).unwrap();
    }
}

fn counted(cache: &ReadCache, before: RetainedHistoryReadCounts) -> RetainedHistoryReadCounts {
    cache.1.snapshot().checked_delta(before).unwrap()
}

#[test]
fn incremental_reader_keeps_old_snapshots_and_publishes_only_valid_suffixes() {
    let mut fixture = Receipts::new();
    fixture.append(1);
    let cache = ReadCache::new();
    let before = cache.1.snapshot();
    assert_eq!(before, RetainedHistoryReadCounts::default());
    let first = cache.read(&fixture.paths).unwrap();
    let cold = counted(&cache, before);
    assert_eq!(
        cold,
        RetainedHistoryReadCounts {
            empty: RetainedHistoryReadOutcomes {
                lookups: 1,
                rebuilt: 1,
                ..Default::default()
            },
            journal_read_attempts: 1,
            replay_records_attempted: 1,
            replay_records_accepted: 1,
            publications: 1,
            ..Default::default()
        }
    );
    assert!(before.checked_delta(cache.1.snapshot()).is_none());
    let before = cache.1.snapshot();
    assert!(Arc::ptr_eq(&first, &cache.read(&fixture.paths).unwrap()));
    assert_eq!(
        counted(&cache, before),
        RetainedHistoryReadCounts {
            same_directory: RetainedHistoryReadOutcomes {
                lookups: 1,
                unmoved: 1,
                ..Default::default()
            },
            ..Default::default()
        }
    );
    fixture.append(2);
    let before = cache.1.snapshot();
    let second = cache.read(&fixture.paths).unwrap();
    assert_eq!(
        counted(&cache, before),
        RetainedHistoryReadCounts {
            same_directory: RetainedHistoryReadOutcomes {
                lookups: 1,
                extended: 1,
                ..Default::default()
            },
            journal_read_attempts: 1,
            replay_records_attempted: 1,
            replay_records_accepted: 1,
            publications: 1,
            ..Default::default()
        }
    );
    assert_eq!(first.1, 1);
    assert_eq!(second.1, 2);
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        second.0[&[1; 32]].frontier.digest(),
        fixture.frontier.digest()
    );

    fixture.append(3);
    fixture.journal.append("unknown.receipt", vec![0]).unwrap();
    let before = cache.1.snapshot();
    for _ in 0..2 {
        assert!(cache.read(&fixture.paths).is_err());
        assert!(Arc::ptr_eq(
            &second,
            &cache.0.lock().unwrap().as_ref().unwrap().history,
        ));
    }
    assert_eq!(
        counted(&cache, before),
        RetainedHistoryReadCounts {
            same_directory: RetainedHistoryReadOutcomes {
                lookups: 2,
                extended: 2,
                ..Default::default()
            },
            journal_read_attempts: 2,
            replay_records_attempted: 4,
            replay_records_accepted: 2,
            replay_failures: 2,
            ..Default::default()
        }
    );
}

#[test]
fn warm_reader_rechecks_disk_bytes_and_enrollment() {
    use std::io::{Seek, SeekFrom, Write};
    let mut fixture = Receipts::new();
    fixture.append(1);
    let cache = ReadCache::new();
    let warm = cache.read(&fixture.paths).unwrap();
    assert!(Arc::ptr_eq(&warm, &cache.read(&fixture.paths).unwrap()));
    assert_eq!(read(&fixture.paths).unwrap().1, 1);
    let file = fixture.paths.dir.join("frontiers.astream");
    let bytes = std::fs::read(&file).unwrap();
    // Same inode, length and checkpoint: byte comparison must still detect the damage.
    let mut opened = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    opened.seek(SeekFrom::End(-1)).unwrap();
    opened.write_all(&[bytes[bytes.len() - 1] ^ 1]).unwrap();
    let before = cache.1.snapshot();
    assert!(cache.read(&fixture.paths).is_err());
    assert_eq!(
        counted(&cache, before),
        RetainedHistoryReadCounts {
            same_directory: RetainedHistoryReadOutcomes {
                lookups: 1,
                errors: 1,
                ..Default::default()
            },
            journal_read_attempts: 1,
            ..Default::default()
        }
    );
    assert!(Arc::ptr_eq(
        &warm,
        &cache.0.lock().unwrap().as_ref().unwrap().history
    ));
    assert!(read(&fixture.paths).is_err());
    opened.seek(SeekFrom::Start(0)).unwrap();
    opened.write_all(&bytes).unwrap();
    assert_eq!(read(&fixture.paths).unwrap().1, 1);

    let checkpoint = fixture.paths.dir.join("frontiers.checkpoint");
    let held = fixture.paths.dir.join("held.checkpoint");
    std::fs::rename(&checkpoint, &held).unwrap();
    let before = cache.1.snapshot();
    assert!(cache.read(&fixture.paths).is_err());
    assert_eq!(
        counted(&cache, before),
        RetainedHistoryReadCounts {
            same_directory: RetainedHistoryReadOutcomes {
                lookups: 1,
                errors: 1,
                ..Default::default()
            },
            journal_read_attempts: 1,
            ..Default::default()
        }
    );
    assert!(read(&fixture.paths).is_err());
    std::fs::rename(&held, &checkpoint).unwrap();
    aterm_journal::write_private_durable(&fixture.paths.dir, "records.frontiers", PENDING).unwrap();
    assert!(read(&fixture.paths).is_err());
    assert_eq!(read_history(&fixture.paths, true).unwrap().1, 1);
    aterm_journal::write_private_durable(&fixture.paths.dir, "records.frontiers", READY).unwrap();
    assert_eq!(read(&fixture.paths).unwrap().1, 1);
}

#[test]
fn reader_retains_only_one_path_and_rebuilds_after_replacement() {
    let mut first = Receipts::new();
    let mut second = Receipts::new();
    first.append(1);
    second.append(1);
    second.append(2);
    let cache = ReadCache::new();
    let first_read = cache.read(&first.paths).unwrap();
    assert_eq!(cache.read(&second.paths).unwrap().1, 2);
    assert_eq!(
        cache.0.lock().unwrap().as_ref().unwrap().dir,
        second.paths.dir
    );
    let rebuilt = cache.read(&first.paths).unwrap();
    assert_eq!(rebuilt.1, first_read.1);
    assert!(!Arc::ptr_eq(&rebuilt, &first_read));
    assert_eq!(
        cache.1.snapshot(),
        RetainedHistoryReadCounts {
            empty: RetainedHistoryReadOutcomes {
                lookups: 1,
                rebuilt: 1,
                ..Default::default()
            },
            other_directory: RetainedHistoryReadOutcomes {
                lookups: 2,
                rebuilt: 2,
                ..Default::default()
            },
            journal_read_attempts: 3,
            replay_records_attempted: 4,
            replay_records_accepted: 4,
            publications: 3,
            replacements: 2,
            ..Default::default()
        }
    );

    // Identical acknowledged contents at a new inode require a fresh decoded snapshot.
    let file = first.paths.dir.join("frontiers.astream");
    let bytes = std::fs::read(&file).unwrap();
    fsx::write_atomic(&file, &bytes, 0o600).unwrap();
    let before = cache.1.snapshot();
    let replaced = cache.read(&first.paths).unwrap();
    assert_eq!(replaced.1, 1);
    assert!(!Arc::ptr_eq(&rebuilt, &replaced));
    assert_eq!(
        counted(&cache, before),
        RetainedHistoryReadCounts {
            same_directory: RetainedHistoryReadOutcomes {
                lookups: 1,
                rebuilt: 1,
                ..Default::default()
            },
            journal_read_attempts: 1,
            replay_records_attempted: 1,
            replay_records_accepted: 1,
            publications: 1,
            ..Default::default()
        }
    );
}

#[test]
fn an_unavailable_cache_still_validates_and_replays_without_publishing() {
    let mut fixture = Receipts::new();
    fixture.append(1);
    let cache = ReadCache::new();
    assert!(
        std::panic::catch_unwind(|| {
            let _held = cache.0.lock().unwrap();
            panic!("poison this fixture's decoded cache");
        })
        .is_err()
    );
    assert_eq!(cache.read(&fixture.paths).unwrap().1, 1);
    assert_eq!(
        cache.1.snapshot(),
        RetainedHistoryReadCounts {
            unavailable: RetainedHistoryReadOutcomes {
                lookups: 1,
                rebuilt: 1,
                ..Default::default()
            },
            journal_read_attempts: 1,
            replay_records_attempted: 1,
            replay_records_accepted: 1,
            ..Default::default()
        }
    );
}

#[test]
fn concurrent_read_counts_conserve_joined_work_without_assuming_cache_order() {
    let mut first = Receipts::new();
    let mut second = Receipts::new();
    first.append(1);
    second.append(1);
    let cache = ReadCache::new();
    std::thread::scope(|threads| {
        let left = threads.spawn(|| {
            for _ in 0..8 {
                assert_eq!(cache.read(&first.paths).unwrap().1, 1);
            }
        });
        let right = threads.spawn(|| {
            for _ in 0..8 {
                assert_eq!(cache.read(&second.paths).unwrap().1, 1);
            }
        });
        left.join().unwrap();
        right.join().unwrap();
    });
    let counts = cache.1.snapshot();
    let rows = [
        counts.empty,
        counts.same_directory,
        counts.other_directory,
        counts.unavailable,
    ];
    let unmoved = rows.iter().map(|row| row.unmoved).sum::<u64>();
    assert_eq!(counts.journal_read_attempts, 16 - unmoved);
    assert_eq!(rows.iter().map(|row| row.lookups).sum::<u64>(), 16);
    assert_eq!(
        rows.iter()
            .map(|row| row.unmoved + row.unchanged + row.rebuilt)
            .sum::<u64>(),
        16
    );
    assert!(rows.iter().all(|row| row.extended == 0 && row.errors == 0));
    let rebuilt = rows.iter().map(|row| row.rebuilt).sum::<u64>();
    assert_eq!(counts.publications, rebuilt);
    assert_eq!(counts.replay_records_attempted, rebuilt);
    assert_eq!(counts.replay_records_accepted, rebuilt);
    assert_eq!(counts.replay_failures, 0);
    assert!(counts.replacements >= 1 && counts.replacements < counts.publications);
}

/// A reader that asks again and again of a journal nobody wrote (the Commons control session's
/// peer checks, a policy check per table) validates it once: at the owner's size, a long history
/// of 2,000 receipts, a hundred reads read the journal's acknowledged bytes once. A file written
/// with the same bytes (the claim saved again) moves its stamps, so the next read validates the
/// journal again, finds it unchanged, and the read after it reads nothing again; an append is
/// read at the next read.
///
/// NEGATIVE CONTROL (2026-11-06): with the stamps never compared (every read validated, as
/// before), the hundred reads read the journal a hundred times (`left: 100, right: 1`).
#[test]
fn a_journal_nobody_wrote_is_validated_once() {
    let mut fixture = Receipts::new();
    for n in 1..=2000 {
        fixture.append(n);
    }
    let cache = ReadCache::new();
    let before = cache.1.snapshot();
    let first = cache.read(&fixture.paths).unwrap();
    assert_eq!(first.1, 2000);
    for _ in 1..100 {
        assert!(Arc::ptr_eq(&first, &cache.read(&fixture.paths).unwrap()));
    }
    let counts = counted(&cache, before);
    assert_eq!(counts.journal_read_attempts, 1);
    assert_eq!(counts.same_directory.unmoved, 99);

    aterm_journal::write_private_durable(&fixture.paths.dir, "records.frontiers", READY).unwrap();
    let before = cache.1.snapshot();
    for _ in 0..2 {
        assert!(Arc::ptr_eq(&first, &cache.read(&fixture.paths).unwrap()));
    }
    let counts = counted(&cache, before);
    assert_eq!(
        (
            counts.same_directory.unchanged,
            counts.same_directory.unmoved
        ),
        (1, 1),
        "validated again once a stamp moved, and not again after"
    );

    fixture.append(2001);
    assert_eq!(cache.read(&fixture.paths).unwrap().1, 2001);
}

pub(crate) fn writer_replay_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let variable = match action {
        "OtherDirectory" => "directory",
        "Advance" => "prefix",
        "LoseCustody" => "usable",
        "Evict" => "cached",
        "Replay" => {
            let mut selected = Receipts::new();
            selected.append(1);
            let cache = ReadCache::new();
            let known = (state["cached"] == 1).then(|| cache.read(&selected.paths).unwrap());
            if state["prefix"] == 0 {
                selected.append(2);
            }
            let expected = selected.frontier.digest();
            let Receipts {
                _dir,
                paths,
                journal,
                ..
            } = selected;
            drop(journal);
            // A distinct spelling of the same directory independently exercises the path
            // check while retaining the same physical identity and checkpoint digest.
            let directory = if state["directory"] == 1 {
                paths.dir.clone()
            } else {
                let child = paths.dir.join("alias");
                fsx::ensure_private_dir(&child).unwrap();
                child.join("..")
            };
            let opened =
                aterm_journal::Log::open(&directory, NAME, ANCHOR, true, limits()).unwrap();
            if state["cached"] == 1 {
                let entry = cache.0.lock().unwrap();
                let entry = entry.as_ref().unwrap();
                assert_eq!(entry.dir == directory, state["directory"] == 1);
                assert_eq!(
                    entry.prefix.acknowledged() == opened.acknowledged(),
                    state["prefix"] == 1,
                );
            }
            if state["usable"] == 0 {
                fsx::write_atomic(&paths.dir.join("frontiers.checkpoint"), b"damaged", 0o600)
                    .unwrap();
            }
            assert_eq!(!opened.is_poisoned(), state["usable"] == 1);
            let replayed = cache.replay(&opened);
            let reused = known
                .as_ref()
                .zip(replayed.as_ref().ok())
                .is_some_and(|(known, replayed)| Arc::ptr_eq(known, replayed));
            if state["usable"] == 1 {
                let replayed = replayed.unwrap();
                assert_eq!(replayed.0[&[1; 32]].frontier.digest(), expected);
                assert_eq!(replayed.1, if state["prefix"] == 1 { 1 } else { 2 });
            } else {
                assert!(replayed.is_err());
            }
            next.insert("phase", 1);
            next.insert("reused", i64::from(reused));
            return next;
        }
        _ => panic!("unknown writer replay action {action}"),
    };
    next.insert(variable, 0);
    next
}

#[test]
fn writer_replay_model_conforms_to_opened_journals() {
    let model = aterm_models_swarm::retained_replay_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = writer_replay_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{action:?}: {state:?} -> {actual:?}"
                );
            }
        }
    }
    let mut lost = model.init_state();
    lost.insert("usable", 0);
    let mut unsafe_reuse = writer_replay_transition(&lost, "Replay");
    unsafe_reuse.insert("reused", 1);
    assert!(!model.check_invariant("CurrentCustody", &unsafe_reuse));
    assert!(!model.successors("Replay", &lost).contains(&unsafe_reuse));
}

#[test]
fn writer_replay_rejects_a_committed_malformed_suffix_after_a_cached_prefix() {
    let mut fixture = Receipts::new();
    fixture.append(1);
    let cache = ReadCache::new();
    let earlier = cache.read(&fixture.paths).unwrap();
    fixture.append(2);
    fixture.journal.append("unknown.receipt", vec![0]).unwrap();
    let Receipts {
        _dir,
        paths,
        journal,
        ..
    } = fixture;
    drop(journal);
    let opened = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, true, limits()).unwrap();
    assert!(cache.replay(&opened).is_err());
    assert_eq!(earlier.1, 1);
    assert!(Arc::ptr_eq(
        &earlier,
        &cache.0.lock().unwrap().as_ref().unwrap().history,
    ));
}

#[test]
#[ignore = "receipt-reader measurement; run explicitly with --ignored --nocapture"]
fn accumulated_receipt_read_cost() {
    use std::{hint::black_box, time::Instant};
    const SAMPLES: usize = 64;
    let mut fixture = Receipts::new();
    for n in 1..=4096 {
        fixture.append(n);
        if ![1, 128, 1024, 4096].contains(&n) {
            continue;
        }
        let expected = fixture.frontier.digest();
        assert_eq!(
            read(&fixture.paths).unwrap().0[&[1; 32]].frontier.digest(),
            expected
        );
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            let history = black_box(read(&fixture.paths).unwrap());
            samples.push(started.elapsed().as_nanos());
            assert_eq!(history.1, n as usize);
            assert_eq!(history.0[&[1; 32]].frontier.digest(), expected);
        }
        samples.sort_unstable();
        eprintln!(
            "RETAINED-READ receipts={n} bytes={} samples={SAMPLES} p50_ns={} p95_ns={} p99_ns={}{}",
            fixture.journal.bytes(),
            samples[SAMPLES / 2],
            samples[60],
            samples[63],
            aterm_durable::durable_flushes_note(),
        );
    }
}

#[test]
#[ignore = "receipt-writer measurement; run explicitly with --ignored --nocapture"]
fn accumulated_receipt_writer_recovery_cost() {
    use std::{hint::black_box, time::Instant};
    const SAMPLES: usize = 64;
    for n in [1, 128, 1024, 4096] {
        let mut fixture = Receipts::new();
        for receipt in 1..=n {
            fixture.append(receipt);
        }
        let expected = fixture.frontier.digest();
        let warm = read(&fixture.paths).unwrap();
        assert_eq!(warm.1, n as usize);
        let Receipts {
            _dir,
            paths,
            journal,
            ..
        } = fixture;
        let bytes = journal.bytes();
        drop(journal);
        let mut opening = Vec::with_capacity(SAMPLES);
        let mut replaying = Vec::with_capacity(SAMPLES);
        let mut total = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            let mut journal =
                aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, true, limits()).unwrap();
            opening.push(started.elapsed().as_nanos());
            let replay_started = Instant::now();
            let history = black_box(replay(&journal).unwrap());
            replaying.push(replay_started.elapsed().as_nanos());
            journal.finish_open().unwrap();
            total.push(started.elapsed().as_nanos());
            assert_eq!(history.1, n as usize);
            assert_eq!(history.0[&[1; 32]].frontier.digest(), expected);
        }
        for (phase, mut samples) in [("open", opening), ("replay", replaying), ("total", total)] {
            samples.sort_unstable();
            eprintln!(
                "RETAINED-WRITER phase={phase} receipts={n} bytes={bytes} samples={SAMPLES} p50_ns={} p95_ns={} p99_ns={}{}",
                samples[SAMPLES / 2],
                samples[60],
                samples[63],
                aterm_durable::durable_flushes_note(),
            );
        }
    }
}
