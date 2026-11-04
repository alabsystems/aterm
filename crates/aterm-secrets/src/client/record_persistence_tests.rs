// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::write_record_buffered;
use std::io::{self, Write};

#[derive(Default)]
struct Sink {
    writes: Vec<usize>,
    bytes: Vec<u8>,
    refuse: bool,
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.refuse {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "disk failed"));
        }
        self.writes.push(bytes.len());
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn record_buffer_coalesces_small_writes_and_flushes_the_final_partial_chunk() {
    let mut sink = Sink::default();
    let count = 3 * 64 * 1024 + 17;
    write_record_buffered(&mut sink, |writer| {
        for _ in 0..count {
            writer.write_all(b"x")?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(sink.bytes, vec![b'x'; count]);
    assert_eq!(sink.writes, [64 * 1024, 64 * 1024, 64 * 1024, 17]);
}

#[test]
fn record_buffer_reports_the_final_write_failure_before_commit() {
    let mut sink = Sink {
        refuse: true,
        ..Sink::default()
    };
    let mut produced = false;
    let result = write_record_buffered(&mut sink, |writer| {
        writer.write_all(b"fits entirely in the buffer")?;
        produced = true;
        Ok(())
    });
    assert!(produced, "failure comes from the explicit final flush");
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert!(sink.bytes.is_empty());
}

fn commons_retention_fixture(
    root: &std::path::Path,
) -> (super::Paths, crate::membership_log::Root) {
    use crate::crossing::fixture::Batch;

    let paths = super::Paths::for_home(root);
    let pin = crate::b64::encode_padded(&crate::envelope::signer_public(&[7; 32]).unwrap());
    super::configure_log_member(
        &paths,
        &format!("{}#{pin}", root.join("team.vault").display()),
        "m1",
    )
    .unwrap();
    let genesis = &Batch::get().base[0];
    super::save_records(&paths, genesis.bytes()).unwrap();
    let crate::membership_log::Body::Genesis { root, .. } = genesis.body() else {
        unreachable!()
    };
    (paths, *root)
}

#[test]
fn expanded_custody_refuses_an_older_local_file_reader_and_writer() {
    use crate::crossing::fixture::{Batch, RAISED};
    use crate::membership_log::{LEGACY_RECORDS, encode_records};

    RAISED.within(|| {
        let home = aterm_tempfile::tempdir().unwrap();
        let (paths, _) = commons_retention_fixture(home.path());
        let batch = Batch::get();
        super::save_records(&paths, &encode_records(&batch.base)).unwrap();
        let legacy_path = paths.records_file();
        let legacy_bytes = std::fs::read(&legacy_path).unwrap();
        super::save_records(&paths, &encode_records(&batch.removal)).unwrap();
        let expected = super::load_records(&paths).unwrap().complete_inventory();
        assert!(expected.len() > LEGACY_RECORDS);

        // This is the old writer's real atomic replacement operation on its captured path.
        assert!(
            crate::fsx::write_atomic(&legacy_path, &legacy_bytes, 0o600).is_err(),
            "an older writer must not replace expanded custody with its earlier snapshot"
        );
        assert!(
            crate::fsx::read_owned(&legacy_path, crate::membership_log::MAX_RECORDS_STREAM)
                .is_err(),
            "an older regular-file reader must not receive expanded custody"
        );
        assert_eq!(
            super::load_records(&paths).unwrap().complete_inventory(),
            expected
        );
    });
}

#[test]
fn crossed_commons_retention_refuses_new_and_already_held_legacy_records() {
    use crate::crossing::fixture::Batch;

    for crossed in [false, true] {
        for record in &Batch::get().base[..2] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (paths, root) = commons_retention_fixture(home.path());
            let before = std::fs::read(paths.records_file()).unwrap();
            if crossed {
                super::crossing_mark(&paths).store().unwrap();
            }
            let retained = super::retain_records_for_commons(
                &paths,
                root.network,
                root.authority,
                &[record.bytes().to_vec()],
            );
            if crossed {
                assert!(retained.is_err(), "a legacy source must be refused");
                assert!(
                    std::fs::read(paths.records_file()).unwrap() == before,
                    "refusal preserves the complete file"
                );
            } else {
                assert_eq!(retained.unwrap(), [true], "the uncrossed control retains");
            }
        }
    }
}

#[test]
fn crossed_commons_retention_rechecks_after_a_writer_interleaves() {
    use crate::crossing::fixture::Batch;

    for crossed_during_read in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (paths, root) = commons_retention_fixture(home.path());
        let before = std::fs::read(paths.records_file()).unwrap();
        let record = &Batch::get().base[1];
        let result = super::retain_records_for_commons_with(
            &paths,
            root.network,
            root.authority,
            &[record.bytes().to_vec()],
            || {
                if crossed_during_read {
                    super::crossing_mark(&paths).store().unwrap();
                }
            },
        );
        if crossed_during_read {
            assert!(result.is_err());
            assert!(std::fs::read(paths.records_file()).unwrap() == before);
        } else {
            assert_eq!(result.unwrap(), [true]);
        }
    }
}

#[test]
fn bounded_commons_marks_new_complete_records_but_not_duplicates_or_waiting() {
    use crate::crossing::{RecordsBound, fixture::Batch};

    let batch = Batch::get();
    for (records, source_crossed, expected, marked, changed) in [
        (vec![&batch.base[0]], true, vec![true], false, false),
        (vec![&batch.base[1]], true, vec![true], true, true),
        (vec![&batch.base[2]], true, vec![false], false, false),
        (vec![&batch.base[2]], false, vec![false], false, true),
        (
            vec![&batch.base[0], &batch.base[2]],
            true,
            vec![true, false],
            false,
            false,
        ),
        (
            vec![&batch.base[1], &batch.base[7]],
            true,
            vec![true, false],
            true,
            true,
        ),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (paths, root) = commons_retention_fixture(home.path());
        let before = std::fs::read(paths.records_file()).unwrap();
        let wires: Vec<_> = records.iter().map(|r| r.bytes().to_vec()).collect();
        let retained = super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            RecordsBound::here(source_crossed),
            &wires,
        )
        .unwrap();
        assert_eq!(retained, expected);
        assert_eq!(super::records_marked_crossed(&paths).unwrap(), marked);
        assert_eq!(
            std::fs::read(paths.records_data_file().unwrap()).unwrap() != before,
            changed
        );
    }
}

#[test]
fn bounded_commons_checks_capacity_even_for_already_held_records() {
    use crate::crossing::{
        RecordsBound,
        fixture::{Batch, LEGACY},
    };

    LEGACY.within(|| {
        for (source, compatible) in [
            (RecordsBound::of(0, false), false),
            (RecordsBound::of(1, false), false),
            (RecordsBound::of(4096, false), true),
            (RecordsBound::of(4096, true), true),
            (RecordsBound::of(8192, false), true),
            (RecordsBound::of(8192, true), false),
        ] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (paths, root) = commons_retention_fixture(home.path());
            let before = std::fs::read(paths.records_file()).unwrap();
            let retained = super::retain_bounded_records_for_commons(
                &paths,
                root.network,
                root.authority,
                source,
                &[Batch::get().base[0].bytes().to_vec()],
            );
            assert_eq!(retained.is_ok(), compatible, "source {source:?}");
            if compatible {
                assert_eq!(retained.unwrap(), [true]);
            }
            assert!(std::fs::read(paths.records_file()).unwrap() == before);
            assert!(!super::records_marked_crossed(&paths).unwrap());
        }
    });
}

#[test]
fn bounded_commons_rechecks_capacity_after_an_interleaving_writer_crosses() {
    use crate::crossing::{
        RecordsBound,
        fixture::{Batch, RAISED},
    };

    RAISED.within(|| {
        for interleave in [false, true] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (paths, root) = commons_retention_fixture(home.path());
            let before = std::fs::read(paths.records_file()).unwrap();
            let retained = super::retain_commons_records_with(
                &paths,
                root.network,
                root.authority,
                &[Batch::get().base[1].bytes().to_vec()],
                Some(RecordsBound::of(4096, false)),
                || {
                    if interleave {
                        super::crossing_mark(&paths).store().unwrap();
                    }
                },
            );
            if interleave {
                assert!(retained.is_err());
                assert!(std::fs::read(paths.records_file()).unwrap() == before);
            } else {
                assert_eq!(retained.unwrap(), [true]);
            }
        }
    });
}

#[test]
fn bounded_commons_mark_failure_or_interruption_precedes_record_custody() {
    use crate::crossing::{Faults, RecordsBound, fixture::Batch, inject};

    for (faults, marked) in [
        (
            Faults {
                fail_store: true,
                ..Faults::default()
            },
            false,
        ),
        (
            Faults {
                dies_between: true,
                ..Faults::default()
            },
            true,
        ),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (paths, root) = commons_retention_fixture(home.path());
        let mark = super::crossing_mark(&paths);
        let before = std::fs::read(paths.records_file()).unwrap();
        inject(mark.path(), faults);
        let retained = super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            RecordsBound::here(true),
            &[Batch::get().base[1].bytes().to_vec()],
        );
        inject(mark.path(), Faults::default());
        assert!(
            retained.is_err(),
            "no acknowledgement after a failed custody write"
        );
        assert!(std::fs::read(paths.records_file()).unwrap() == before);
        assert_eq!(mark.stored().unwrap(), marked);
    }
}

#[test]
fn bounded_commons_reconnect_completes_a_previously_unretained_child() {
    use crate::crossing::{RecordsBound, fixture::Batch};

    let home = aterm_tempfile::tempdir().unwrap();
    let (paths, root) = commons_retention_fixture(home.path());
    let source = RecordsBound::here(true);
    let child = Batch::get().base[2].bytes().to_vec();
    assert_eq!(
        super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            source,
            std::slice::from_ref(&child),
        )
        .unwrap(),
        [false]
    );
    assert!(!super::records_marked_crossed(&paths).unwrap());
    let parent = Batch::get().base[1].bytes().to_vec();
    assert_eq!(
        super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            source,
            &[child.clone(), parent],
        )
        .unwrap(),
        [true, true]
    );
    assert!(super::records_marked_crossed(&paths).unwrap());
    assert_eq!(
        super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            source,
            &[child],
        )
        .unwrap(),
        [true]
    );
}

#[test]
fn bounded_commons_rejects_bad_capacity_and_oversized_batches_before_decoding() {
    use crate::crossing::RecordsBound;

    let home = aterm_tempfile::tempdir().unwrap();
    let (paths, root) = commons_retention_fixture(home.path());
    let before = std::fs::read(paths.records_file()).unwrap();
    assert!(
        super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            RecordsBound::of(0, false),
            &[vec![0]],
        )
        .is_err(),
        "even an invalid record cannot bypass source compatibility"
    );
    let oversized = vec![Vec::new(); crate::membership_log::RecordBudgets::current().held + 1];
    assert!(
        super::retain_bounded_records_for_commons(
            &paths,
            root.network,
            root.authority,
            RecordsBound::here(false),
            &oversized,
        )
        .is_err()
    );
    assert!(std::fs::read(paths.records_file()).unwrap() == before);
    assert!(!super::records_marked_crossed(&paths).unwrap());
}

/// This Mac stores its crossed mark ([`crate::crossing`]) before its records file holds more than
/// the legacy bound: where the mark cannot be stored, the write fails and the file keeps what it
/// held. Its store holds the fixture past the legacy bound, so it is built on the test thread at
/// the raised held bound ([`crate::crossing::fixture::RAISED`]) through the budgets seam.
///
/// CONTROL (MUTANT `Faults::mark_after_take`): the file holds the 4,097th record with no mark,
/// where the mark then cannot be stored.
#[test]
fn a_mac_stores_its_mark_before_its_records_file_holds_its_4097th_record() {
    use crate::crossing::fixture::{self, Batch, VAULT};
    use crate::crossing::{Faults, inject};
    use crate::membership_log::{LEGACY_RECORDS, Store};

    let batch = Batch::get();
    let write = |faults: Faults| {
        fixture::RAISED.within(|| {
            let home = aterm_tempfile::tempdir().expect("tempdir");
            let paths = super::Paths::for_home(home.path());
            std::fs::create_dir_all(&paths.dir).expect("the secrets dir");
            let mark = super::crossing_mark(&paths);
            let mut store = Store::for_vault(VAULT).expect("store");
            let mut records = batch.whole();
            let past = records.split_off(LEGACY_RECORDS);
            let _ = store.ingest_records(records).expect("the legacy bound");
            super::write_records(&paths, &store).expect("a file at the legacy bound");
            assert!(!mark.stored().expect("look"), "no mark at the legacy bound");
            let before = std::fs::read(paths.records_file()).expect("the file");
            let _ = store
                .ingest_records(past[..1].to_vec())
                .expect("the 4,097th record");
            inject(mark.path(), faults);
            let written = super::write_records(&paths, &store);
            inject(mark.path(), Faults::default());
            let after = std::fs::read(paths.records_data_file().unwrap()).expect("the file");
            (
                written.is_ok(),
                after != before,
                mark.stored().expect("look"),
            )
        })
    };

    assert_eq!(
        write(Faults::default()),
        (true, true, true),
        "the mark, then the file past the legacy bound"
    );
    assert_eq!(
        write(Faults {
            fail_store: true,
            ..Faults::default()
        }),
        (false, false, false),
        "no mark, and the file keeps what it held"
    );
    assert_eq!(
        write(Faults {
            fail_store: true,
            mark_after_take: true,
            ..Faults::default()
        }),
        (false, true, false),
        "mutant: the file holds the 4,097th record with no mark"
    );
}
