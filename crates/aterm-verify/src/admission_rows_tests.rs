// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run's admission-log rows ([`super`]): a stage's row says its claim, what sized it (and the
//! width of the record that did), the compile width and the stage's peak; a test run's row sums
//! its binaries by width class; and each run of the checks writes a `run` row of its own. Every
//! claim is in a scratch ledger on a fixed machine, captured as this user's
//! ([`aterm_capacity::admission_log::capture`]): no row reaches this account's log.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Reading};

use crate::ladder::Report;
use crate::memory::{Book, StageKey as _};
use crate::plan::{StageId, StageSpec};
use crate::{Ctx, EnvSnapshot, Scope, Seams};

const GIB: u64 = 1 << 30;

/// The scratch ledger this file's admissions are made in.
static LEDGER: OnceLock<PathBuf> = OnceLock::new();

/// Admit `need` in the scratch ledger on a 24 GiB machine with 20 GiB available.
fn admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    crate::admit_in(
        &Ledger::at(LEDGER.get().expect("the test set its ledger")),
        &Fixed::new(Reading::of_gib(24, 20, 14)),
        Pace {
            poll: std::time::Duration::from_millis(20),
            ..Pace::default()
        },
        need,
        mode,
        &mut |_| {},
    )
}

fn unused(_: &Ctx, spec: &StageSpec) -> Report {
    Report::new(&spec.title)
}

/// Run stage `spec` once in `ctx` through [`crate::staged`], its callback booking a 6 GiB peak
/// and two seconds of CPU into the stage's meter: its `stage` rows and the ids of the claims
/// admitted meanwhile.
fn staged_rows(
    ctx: &Ctx,
    spec: &StageSpec,
) -> (Vec<aterm_capacity::admission_log::Row>, Vec<String>) {
    let quiet = Mutex::new(crate::Quiet {
        claim: None,
        left: 1,
        admit,
    });
    let early = crate::EarlyClaims::default();
    let ledger = Ledger::at(LEDGER.get().expect("set"));
    let (_, rows) = aterm_capacity::admission_log::capture(ledger, || {
        crate::staged(
            ctx,
            &quiet,
            spec,
            None,
            Seams {
                stage: unused,
                prepare_cells: None,
                prepare_build_dir: |_| Ok(()),
                admit_quiet: admit,
                admit_memory: admit,
            },
            &early,
            || {
                crate::memory::current().expect("a metered stage").book(
                    4242,
                    &[aterm_sysprobe::MemberUsage {
                        pid: 4242,
                        start: 1,
                        cpu_ns: 2_000_000_000,
                        runnable_ns: 2_000_000_000,
                        io_bytes: 0,
                        footprint: 6 * GIB,
                        peak_footprint: 6 * GIB,
                    }],
                );
                let mut report = Report::new(&spec.title);
                report.pass("ran");
                report
            },
        )
    });
    let admitted = rows
        .iter()
        .filter(|r| r.kind() == "admitted")
        .filter_map(|r| r.str("id").map(str::to_owned))
        .collect();
    let stages = rows.into_iter().filter(|r| r.kind() == "stage").collect();
    (stages, admitted)
}

/// A lint stage claimed by its own kept 5 GiB peak: its row names the run and the claim the
/// ledger admitted and its bytes, says `learned` for what sized it, by no width-keyed record, the
/// host's width for an unpooled compile, the 6 GiB the meter read and that its record kept it.
/// NEGATIVE CONTROLS: the same stage under `--admit now` says `now`; a stage row with no meter
/// says no peak, and one with no claim says `none`.
#[test]
fn a_stage_row_carries_claim_basis_width_and_peak() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    LEDGER
        .set(dir.path().join("ledger"))
        .expect("this file's one ledger");
    let scope = Scope::crate_only("aterm-gui");
    let ctx = |mode: Admit| {
        Ctx::new(
            dir.path().to_path_buf(),
            scope.clone(),
            EnvSnapshot::default(),
            dir.path().join("scratch"),
        )
        .with_memory(Book::at(
            Some(dir.path().join("stage-memory.json")),
            Some(24 * GIB),
            Some(43 * GIB / 2),
        ))
        .with_quiet(Some(mode))
    };
    let waits = ctx(Admit::Wait);
    waits.admission.begin();
    let mut spec = crate::plan::declared(&scope)
        .into_iter()
        .find(|s| s.id == StageId::Tippy)
        .expect("declared");
    let book = waits.memory.as_ref().expect("a book");
    let key = crate::memory::Key::of_stage(&spec, &waits);
    book.keep(&spec, &key, 5 * GIB).expect("kept");
    let need = book.need(&spec, &key, None);
    assert!(matches!(need.how, crate::memory::How::Learned(_)), "{need}");
    spec.mem = need.bytes;

    let (rows, admitted) = staged_rows(&waits, &spec);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.str("stage"), Some("Tippy"));
    assert_eq!(row.str("basis"), Some("learned"));
    assert_eq!(
        row.u64("keyed_jobs"),
        None,
        "an unpooled compile's record has no width"
    );
    assert_eq!(row.u64("claimed"), Some(need.bytes));
    assert_eq!(admitted.len(), 1, "one claim admitted: {admitted:?}");
    assert_eq!(row.str("claim"), Some(admitted[0].as_str()));
    assert_eq!(row.u64("width"), Some(u64::from(waits.parallelism())));
    assert!(!row.flag("pooled"));
    assert_eq!(row.u64("peak"), Some(6 * GIB));
    assert_eq!(row.str("kept"), Some("kept"));
    assert_eq!(row.u64("cpu_ms"), Some(2_000));
    assert!(waits.admission.id().is_some());
    assert_eq!(row.str("run"), waits.admission.id().as_deref());

    let (forced, _) = staged_rows(&ctx(Admit::Now), &spec);
    assert_eq!(forced.len(), 1, "{forced:?}");
    assert_eq!(forced[0].str("basis"), Some("now"));

    let bare = super::stage_row(&waits, &spec, None, None, None, None, Instant::now());
    assert_eq!(bare.u64("peak"), None);
    assert_eq!(bare.str("basis"), Some("none"));
    assert_eq!(bare.str("kept"), None);
}

/// A test run's row sums its binaries' claims and peaks by width class: a binary claimed by a
/// peak its history read at the width it ran at is `matched`, at another width `mismatched`, and
/// one with no kept peak (the fallback) `unseen`; a binary that never started counts nowhere.
#[test]
fn a_tests_row_sums_binaries_by_width_class() {
    use crate::test_history::{History, Sample};
    let sample = |width: u32, peak: u64| Sample {
        at_ms: 1,
        outcome: "ok".to_owned(),
        wall_ms: 1_000,
        cpu_ms: Some(1_000),
        starved_ms: Some(0),
        peak_bytes: Some(peak),
        width,
        cores: Some(1),
        tests: Some(1),
        partition: None,
    };
    let mut history = History::default();
    history.push("-p a --lib", sample(4, 2 * GIB));
    history.push("-p b --lib", sample(8, 3 * GIB));
    let bins: Vec<crate::testrun::Binary> =
        ["-p a --lib", "-p b --lib", "-p c --lib", "-p d --lib"]
            .iter()
            .map(|spec| crate::testrun::Binary {
                header: format!("Running {spec}"),
                spec: (*spec).to_owned(),
                record: crate::testrun::Record {
                    cwd: PathBuf::from("/"),
                    argv: Vec::new(),
                    env: Vec::new(),
                },
                harness: true,
            })
            .collect();
    let ran = |width: u32, claim: u64, peak: u64, started: bool| {
        Some(crate::testrun::Ran {
            run: crate::exec::Run {
                usage: Some(crate::exec::TreeUsage {
                    cpu: std::time::Duration::from_millis(500),
                    starved: std::time::Duration::from_millis(100),
                    peak_bytes: peak,
                    ..crate::exec::TreeUsage::default()
                }),
                ..crate::exec::Run::default()
            },
            started,
            planned: crate::test_plan::Planned {
                index: 0,
                width,
                charge: 1,
                peak: claim,
                predicted: None,
            },
            waiting: std::time::Duration::ZERO,
            execution: std::time::Duration::from_secs(1),
        })
    };
    let rows = [
        ran(4, 2 * GIB, GIB, true),
        ran(4, 3 * GIB, 4 * GIB, true),
        ran(4, 16 * GIB, GIB, true),
        ran(4, 16 * GIB, GIB, false),
    ];
    let row = super::tests_row(
        Some("4242-1-1".to_owned()),
        "test",
        &bins,
        &rows,
        &history,
        super::Recording {
            claim: Some(17 * GIB),
            peak: Some(5 * GIB),
        },
    );
    let class = |name: &str, field: &str| {
        row.get("classes")
            .and_then(|c| c.get(name))
            .and_then(|c| c.get(field))
            .and_then(aterm_json::Value::as_u64)
    };
    assert_eq!(class("matched", "count"), Some(1));
    assert_eq!(class("matched", "over"), Some(1), "2 GiB claimed, 1 held");
    assert_eq!(class("mismatched", "count"), Some(1));
    assert_eq!(class("mismatched", "under"), Some(1));
    assert_eq!(class("mismatched", "under_bytes"), Some(GIB));
    assert_eq!(
        class("unseen", "count"),
        Some(1),
        "the unstarted one counts nowhere"
    );
    assert_eq!(row.str("run"), Some("4242-1-1"));
    assert_eq!(row.u64("binaries"), Some(3));
    assert_eq!(row.u64("cpu_ms"), Some(1_500));
    assert_eq!(row.u64("starved_ms"), Some(300));
    assert_eq!(row.u64("recording_claim"), Some(17 * GIB));
    assert_eq!(row.u64("recording_peak"), Some(5 * GIB));
}

/// A pooled compile's claim says the width of the record that sized it only where that record is
/// the one keyed by its compiler jobs: with only the host-width record kept, a run pooled at six
/// jobs is sized by it, and says no width; once six jobs' own record keeps a peak, it is sized by
/// that one, and says six. NEGATIVE CONTROL: an unpooled lookup says no width either way.
#[test]
fn a_pooled_claim_says_which_width_sized_it() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let scope = Scope::crate_only("aterm-gui");
    let ctx = Ctx::new(
        dir.path().to_path_buf(),
        scope.clone(),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_memory(Book::at(
        Some(dir.path().join("stage-memory.json")),
        Some(24 * GIB),
        Some(43 * GIB / 2),
    ));
    let spec = crate::plan::declared(&scope)
        .into_iter()
        .find(|s| s.id == StageId::Tippy)
        .expect("declared");
    let book = ctx.memory.as_ref().expect("a book");
    let key = crate::memory::Key::of_stage(&spec, &ctx);
    book.keep(&spec, &key, 10 * GIB)
        .expect("the host-width peak");
    let (host, keyed) = book.need_keyed(&spec, &key, Some(6));
    assert!(matches!(host.how, crate::memory::How::Learned(_)), "{host}");
    assert_eq!(host.bytes, aterm_capacity::peak_claim(10 * GIB));
    assert_eq!(keyed, None, "sized by the record no width keys");

    let pooled = crate::compile_width::record_key(key.clone(), Some(6));
    book.keep(&spec, &pooled, 6 * GIB)
        .expect("six jobs' own peak");
    let (own, keyed) = book.need_keyed(&spec, &key, Some(6));
    assert_eq!(own.bytes, aterm_capacity::peak_claim(6 * GIB));
    assert_eq!(keyed, Some(6), "sized by six jobs' own record");
    let row_basis = super::basis(
        &crate::plan::StageSpec {
            mem: own.bytes,
            ..spec.clone()
        },
        &own,
        keyed,
    );
    assert_eq!(row_basis.word, "learned");
    assert_eq!(row_basis.keyed_jobs, Some(6));

    let (_, unpooled) = book.need_keyed(&spec, &key, None);
    assert_eq!(unpooled, None);
}

/// Each run of a context's checks is a run of its own, as `verify release-input` runs one a check
/// in one process: two runs of one context write two `run` rows, under two ids, each saying the
/// code its run ended with, which run ends early (its build folder could not be prepared) or not.
#[test]
fn each_run_of_the_checks_writes_its_own_run_row() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ctx = Ctx::new(
        dir.path().to_path_buf(),
        Scope::crate_only("aterm-gui"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_checks(&[crate::checks::Check::Fmt]);
    let seams = Seams {
        stage: unused,
        prepare_cells: None,
        prepare_build_dir: |_| Err(std::io::Error::other("no build folder in this test")),
        admit_quiet: admit,
        admit_memory: admit,
    };
    let ledger = Ledger::at(dir.path().join("ledger"));
    let (codes, rows) = aterm_capacity::admission_log::capture(ledger, || {
        (0..2)
            .map(|_| {
                crate::run_with(
                    &ctx,
                    &mut std::io::sink(),
                    &mut std::io::sink(),
                    &crate::Show::default(),
                    seams,
                )
                .expect("written")
            })
            .collect::<Vec<i32>>()
    });
    let runs: Vec<_> = rows.iter().filter(|r| r.kind() == "run").collect();
    assert_eq!(runs.len(), 2, "{rows:?}");
    assert_ne!(runs[0].str("run"), runs[1].str("run"), "two runs, two ids");
    for (row, code) in runs.iter().zip(&codes) {
        assert!(row.str("run").is_some(), "{row:?}");
        assert_eq!(
            row.get("code").and_then(aterm_json::Value::as_i64),
            Some(i64::from(*code))
        );
        assert_eq!(
            row.get("checks")
                .and_then(aterm_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            row.get("land").and_then(aterm_json::Value::as_bool),
            Some(false),
            "no landing judged these runs"
        );
    }
    // A run `verify land` judges says so: the landing's `--queued-since` marks the process.
    ctx.admission.judged_by_land();
    let (_, rows) =
        aterm_capacity::admission_log::capture(Ledger::at(dir.path().join("ledger-land")), || {
            crate::run_with(
                &ctx,
                &mut std::io::sink(),
                &mut std::io::sink(),
                &crate::Show::default(),
                seams,
            )
            .expect("written")
        });
    let judged: Vec<_> = rows.iter().filter(|r| r.kind() == "run").collect();
    assert_eq!(judged.len(), 1, "{rows:?}");
    assert_eq!(
        judged[0].get("land").and_then(aterm_json::Value::as_bool),
        Some(true)
    );
}
