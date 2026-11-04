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
    use crate::crossing::fixture::Batch;
    use crate::membership_log::{LEGACY_RECORDS, encode_records};

    let home = aterm_tempfile::tempdir().unwrap();
    let (paths, _) = commons_retention_fixture(home.path());
    let batch = Batch::get();
    super::save_records(&paths, &encode_records(&batch.base)).unwrap();
    let legacy_path = paths.records_file();
    let legacy_bytes = std::fs::read(&legacy_path).unwrap();
    super::save_records(&paths, &encode_records(&batch.removal)).unwrap();
    let expected: std::collections::BTreeSet<_> = batch
        .base
        .iter()
        .chain(&batch.removal)
        .map(|record| *record.id())
        .collect();
    assert!(expected.len() > LEGACY_RECORDS);
    let actual = super::load_records(&paths).unwrap().complete_inventory();
    assert!(
        actual == expected,
        "retained {} of {} required records under the production budget",
        actual.len(),
        expected.len()
    );

    // This is the old writer's real atomic replacement operation on its captured path.
    assert!(
        crate::fsx::write_atomic(&legacy_path, &legacy_bytes, 0o600).is_err(),
        "an older writer must not replace expanded custody with its earlier snapshot"
    );
    assert!(
        crate::fsx::read_owned(&legacy_path, crate::membership_log::MAX_RECORDS_STREAM).is_err(),
        "an older regular-file reader must not receive expanded custody"
    );
    let (_, fresh) = super::records_store(&paths).unwrap();
    let mut reader = crate::fsx::open_owned_reader(&paths.records_data_file().unwrap())
        .unwrap()
        .unwrap();
    let reopened = fresh.recovering_read(reader.file()).unwrap();
    assert_eq!(reopened.complete_inventory(), expected);
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

/// A Mac replaying `SwarmRecordsCapacity`'s `Mac` actions on its own home, on the test thread
/// within the configuration's budgets. `Mac` is a Mac whose records file holds the crossing
/// fixture's base log ([`crate::crossing::fixture::Batch::base`]). Each action reads its store
/// from the file, as a pass does, takes the removal batch, and writes it: `MacPulls` as a pull
/// writes what a crossed log's server sent ([`super::write_taught_by_crossed`]), `MacPullDies`
/// the same with the Mac dying between its mark and its file (`Faults::dies_between`),
/// `MacTakes` as every other write does ([`super::write_records`]), and `MacTakeFails` the same
/// where the mark cannot be stored (`Faults::fail_store`).
struct MacRun {
    _home: aterm_tempfile::TempDir,
    paths: super::Paths,
    /// What the run does wrong, beside the fault each action brings.
    mutant: crate::crossing::Faults,
    /// Whether the file took records a crossed log's server sent (the model's `nx`).
    taught: bool,
}

impl MacRun {
    fn start(mutant: crate::crossing::Faults) -> Self {
        use crate::crossing::fixture::{Batch, VAULT};

        let home = aterm_tempfile::tempdir().expect("tempdir");
        let paths = super::Paths::for_home(home.path());
        std::fs::create_dir_all(&paths.dir).expect("the secrets dir");
        let mut store = crate::membership_log::Store::for_vault(VAULT).expect("store");
        let _ = store
            .ingest_records(Batch::get().base.clone())
            .expect("the base");
        super::write_records(&paths, &store).expect("the base file");
        crate::crossing::inject(super::crossing_mark(&paths).path(), mutant);
        Self {
            _home: home,
            paths,
            mutant,
            taught: false,
        }
    }

    /// The store its file holds, read as a pass reads it.
    fn store(&self) -> crate::membership_log::Store {
        let bytes = std::fs::read(self.paths.records_data_file().expect("the records layout"))
            .expect("the file");
        crate::membership_log::Store::recover(crate::crossing::fixture::VAULT, &bytes)
            .expect("store")
    }

    /// How many of the removal batch's records its file holds.
    fn batch_held(&self) -> usize {
        let store = self.store();
        crate::crossing::fixture::Batch::get()
            .removal
            .iter()
            .filter(|record| store.holds(record.id()))
            .count()
    }

    /// The removal batch taken and written, from a crossed log's server where `crossed`, the
    /// write suffering `fault` beside the run's mutant.
    fn write(&mut self, crossed: bool, fault: crate::crossing::Faults) {
        let before = self.batch_held();
        let mut store = self.store();
        let _ = store
            .ingest_records(crate::crossing::fixture::Batch::get().removal.clone())
            .expect("the batch");
        let mark = super::crossing_mark(&self.paths);
        let failing = fault.dies_between || fault.fail_store;
        crate::crossing::inject(
            mark.path(),
            crate::crossing::Faults {
                dies_between: fault.dies_between,
                fail_store: fault.fail_store,
                ..self.mutant
            },
        );
        let written = if crossed {
            super::write_taught_by_crossed(&self.paths, &mark, &store)
        } else {
            super::write_records(&self.paths, &store)
        };
        crate::crossing::inject(mark.path(), self.mutant);
        assert_eq!(written.is_err(), failing, "{written:?}");
        if crossed && self.batch_held() > before {
            self.taught = true;
        }
    }

    fn apply(&mut self, action: &str) {
        use crate::crossing::Faults;
        match action {
            "MacPulls" => self.write(true, Faults::default()),
            "MacPullDies" => self.write(
                true,
                Faults {
                    dies_between: true,
                    ..Faults::default()
                },
            ),
            "MacTakes" => self.write(false, Faults::default()),
            "MacTakeFails" => self.write(
                false,
                Faults {
                    fail_store: true,
                    ..Faults::default()
                },
            ),
            other => panic!("not a Mac write: {other}"),
        }
    }
}

impl Drop for MacRun {
    fn drop(&mut self) {
        crate::crossing::inject(
            super::crossing_mark(&self.paths).path(),
            crate::crossing::Faults::default(),
        );
    }
}

/// `SwarmRecordsCapacity`'s variables, read off a real Mac in its `Mac` scenario: `nr` how many
/// of the batch's records its file holds, `nx` whether records a crossed log's server sent are
/// among them, `mark` whether its crossed mark is stored. No legacy database peer has asked it
/// (`fenced` and `gs` 0): `aterm-db`'s conformance judges `LegacyDbHello`.
fn records_capacity_mac(run: &MacRun) -> aterm_spec::interp::State {
    let count = |n: usize| i64::try_from(n).expect("a count");
    aterm_spec::interp::State::from([
        ("sc", 3),
        ("w", 0),
        ("k", 0),
        ("last", 0),
        ("lw", 0),
        ("lk", 0),
        ("nr", count(run.batch_held())),
        ("nx", i64::from(run.taught)),
        (
            "mark",
            i64::from(super::crossing_mark(&run.paths).stored().expect("look")),
        ),
        ("gs", 0),
        ("fenced", 0),
    ])
}

/// The budgets a Mac holds `model`'s `Mac` scenario within: the explicit legacy bound where
/// `CX` past the base is `LX`, and production capacity where it takes the whole batch.
fn mac_budgets(model: &aterm_spec::derive::Model) -> crate::membership_log::RecordBudgets {
    use crate::crossing::fixture::{self, Batch};
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(constant, _)| *constant == name)
            .map(|&(_, value)| usize::try_from(value).expect("a count"))
            .expect("the model's constant")
    };
    assert_eq!(constant("R"), Batch::get().removal.len());
    assert_eq!(
        fixture::BASE + constant("LX"),
        crate::membership_log::LEGACY_RECORDS
    );
    if constant("CX") == constant("LX") {
        fixture::LEGACY
    } else {
        assert_eq!(constant("CX"), Batch::get().removal.len());
        crate::membership_log::RecordBudgets::PRODUCTION
    }
}

/// S8: a Mac's records file and crossed mark refine `SwarmRecordsCapacity`'s `Mac` writes in
/// each configuration the model declares (the legacy held total, where its file takes the batch
/// up to the bound; the activation's, past it): from every state the model reaches by them,
/// every write it enables, replayed on a real Mac (a fresh home each), gives the state the
/// model's walk found by that path. A legacy database peer's exchange changes neither file nor
/// mark and these writes read neither of its variables, so the walk leaves it to `aterm-db`'s
/// conformance, which judges it from every state.
///
/// NEGATIVE CONTROLS, `Faults::mark_after_take` (the file before the mark): at the legacy held
/// total a pull that dies between them leaves the file holding what a crossed log's server sent
/// with no mark, and at the activation's a write whose mark cannot be stored leaves the file
/// past the bound with none. Each is no successor of its action and breaks `CrossedBeforeHeld`.
#[test]
fn a_macs_records_file_conforms_to_the_capacity_model() {
    let name = aterm_models_swarm::records_capacity_model().name;
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configurations| configurations[0].name == name)
        .expect("SwarmRecordsCapacity is registered");
    let writes = ["MacPulls", "MacPullDies", "MacTakes", "MacTakeFails"];
    let mut judged_all = 0;
    for model in &configurations {
        let start = model.successors("Mac", &model.init_state());
        assert_eq!(start.len(), 1);
        let mut paths: std::collections::BTreeMap<aterm_spec::interp::State, Vec<&'static str>> =
            std::collections::BTreeMap::from([(start[0].clone(), Vec::new())]);
        let mut queue = std::collections::VecDeque::from([start[0].clone()]);
        while let Some(state) = queue.pop_front() {
            for action in &model.actions {
                if !writes.contains(&action.name) {
                    continue;
                }
                for next in model.successors(action.name, &state) {
                    if !paths.contains_key(&next) {
                        let mut path = paths[&state].clone();
                        path.push(action.name);
                        paths.insert(next.clone(), path);
                        queue.push_back(next);
                    }
                }
            }
        }
        let budgets = mac_budgets(model);
        budgets.within(|| {
            let run = MacRun::start(crate::crossing::Faults::default());
            assert_eq!(records_capacity_mac(&run), start[0], "after Mac");
            for (state, path) in &paths {
                for action in writes {
                    if !model.action_enabled(action, state) {
                        continue;
                    }
                    let mut run = MacRun::start(crate::crossing::Faults::default());
                    for step in path {
                        run.apply(step);
                    }
                    run.apply(action);
                    let observed = records_capacity_mac(&run);
                    assert_eq!(
                        model.successors(action, state),
                        vec![observed],
                        "{action} after {path:?}"
                    );
                    judged_all += 1;
                }
            }
        });
        eprintln!(
            "SwarmRecordsCapacity Mac (held {}): {} states",
            budgets.held,
            paths.len()
        );
    }
    assert!(judged_all > 0);

    let control = |model: &aterm_spec::derive::Model, action: &str| {
        let start = model.successors("Mac", &model.init_state())[0].clone();
        let observed = mac_budgets(model).within(|| {
            let mut run = MacRun::start(crate::crossing::Faults {
                mark_after_take: true,
                ..crate::crossing::Faults::default()
            });
            run.apply(action);
            records_capacity_mac(&run)
        });
        assert!(
            !model.successors(action, &start).contains(&observed),
            "{action}: {observed:?} is a successor"
        );
        assert!(!model.check_invariant("CrossedBeforeHeld", &observed));
        observed
    };
    let died = control(&configurations[0], "MacPullDies");
    assert_eq!(
        (died["nr"], died["nx"], died["mark"]),
        (3, 1, 0),
        "mutant: what a crossed log's server sent is held with no mark"
    );
    let failed = control(&configurations[1], "MacTakeFails");
    assert_eq!(
        (failed["nr"], failed["mark"]),
        (5, 0),
        "mutant: the file passes the legacy bound with no mark"
    );
}
