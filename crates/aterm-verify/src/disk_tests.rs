// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn nonincremental_observations_cannot_bound_incremental_runs() {
    let ctx = Ctx::new(
        PathBuf::from("/w/aterm"),
        crate::Scope::crate_only("a"),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/x"),
    )
    .with_checks(&[Check::Lint, Check::Test]);
    let lean = selection_of(&ctx);
    let mut old = lean.clone();
    old.work = old.work.replace(" --no-incremental", "");
    assert!(
        old.holds(&lean),
        "the old larger layout remains an upper bound"
    );
    assert!(
        !lean.holds(&old),
        "negative control: fewer bytes cannot bound the old layout"
    );
    let mut records = aterm_capacity::MachineFootprints::default();
    let wrote = |bytes| atpkg::admit_disk::Footprint {
        peak: bytes,
        grew: bytes,
        at_ms: 1,
        dirs: Some(1),
        scope: None,
    };
    records.push(old.clone(), wrote(49 * GIB));
    assert_eq!(records.cover(&lean, 0, COLD_BYTES).unwrap().bytes, 49 * GIB);
    records.push(lean.clone(), wrote(8 * GIB));
    assert_eq!(records.cover(&lean, 0, COLD_BYTES).unwrap().bytes, 8 * GIB);
    assert_eq!(records.cover(&old, 0, COLD_BYTES).unwrap().bytes, 49 * GIB);
}

#[test]
fn only_a_planned_compiler_claims_build_disk() {
    let measure = || {
        Ctx::new(
            PathBuf::from("/w/aterm"),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(&[Check::Measure])
    };
    let app = || Some(crate::MeasureArtifact::unnamed("/release/aterm.app"));
    assert!(
        run_of(
            &measure()
                .with_measure_artifact(app())
                .with_cursor_pacing(true)
        )
        .unwrap()
        .is_none()
    );
    assert!(run_of(&measure()).unwrap().is_none());
    // Negative controls: compiling benches still needs disk, including a prebuild without
    // a bundle and the headless matrices left by --no-windows.
    assert!(
        run_of(&measure().with_compile_only(true))
            .unwrap()
            .is_some()
    );
    assert!(
        run_of(&measure().with_measure_artifact(app()))
            .unwrap()
            .is_some()
    );
    assert!(
        run_of(&measure().with_measure_artifact(app()).with_no_windows(true))
            .unwrap()
            .is_some()
    );
}

/// The build directory a value names, as targo's `--config` spells it, for the cache and the
/// pinned target alike; anything else names none.
#[test]
fn a_build_dir_value_names_its_directory() {
    let root = Path::new("/Users//u/aterm");
    assert_eq!(
        build_dir_path(
            "build.build-dir='/Users//u/Library/Caches/aterm/cargo-build.noindex{workspace-root}/.cargo-build'",
            root
        ),
        Some(PathBuf::from(
            "/Users//u/Library/Caches/aterm/cargo-build.noindex/Users//u/aterm/.cargo-build"
        ))
    );
    assert_eq!(
        build_dir_path("build.build-dir='/Volumes/s/target/build.noindex'", root),
        Some(PathBuf::from("/Volumes/s/target/build.noindex"))
    );
    assert_eq!(build_dir_path("target-dir='/x'", root), None);
}

/// A run that builds claims its build directory and its corner of the target; one that only
/// reads the tree claims nothing.
#[test]
fn only_a_run_that_builds_claims_disk() {
    let ctx = || {
        Ctx::new(
            PathBuf::from("/w/aterm"),
            crate::Scope::packages(&["aterm-capacity".to_owned()]),
            crate::EnvSnapshot {
                cargo_target_dir: Some(PathBuf::from("/Volumes/s/target")),
                ..crate::EnvSnapshot::default()
            },
            PathBuf::from("/tmp/x"),
        )
    };
    let lint = ctx().with_checks(&[Check::Fmt, Check::Lint]);
    let run = run_of(&lint).unwrap().expect("lint builds");
    assert_eq!(run.cold, COLD_BYTES);
    assert_eq!(run.on, PathBuf::from("/Volumes/s/target"));
    assert_eq!(
        run.dirs.last(),
        Some(&PathBuf::from("/Volumes/s/target/verify"))
    );
    assert_eq!(
        run.record,
        PathBuf::from("/Volumes/s/target/verify/footprint.json")
    );
    assert!(run.what.contains("lint") && run.what.contains("aterm-capacity"));
    let reads = ctx().with_checks(&[Check::Fmt, Check::Census, Check::Forge, Check::ExportScan]);
    assert_eq!(run_of(&reads).unwrap(), None);
}

/// The run's disk claim, given up behind a holder that made no progress, is the run's COULD
/// NOT RUN, naming the holder and how long it made none (`atpkg::admit_disk`'s tests hold the
/// wait to that end, which [`run_of`] asks for). NEGATIVE CONTROL: a claim the ledger could
/// not take runs without one, as a stuck wait would with no answer of its own.
#[test]
fn a_disk_claim_given_up_behind_a_stuck_holder_could_not_run() {
    let ctx = Ctx::new(
        PathBuf::from("/w/aterm"),
        crate::Scope::packages(&["aterm-capacity".to_owned()]),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/x"),
    )
    .with_checks(&[Check::Lint]);
    assert_eq!(
        run_of(&ctx).unwrap().and_then(|r| r.stuck_ends),
        Some(crate::STUCK_ENDS)
    );
    let span = std::time::Duration::from_secs(16 * 60);
    let holder = aterm_capacity::Holder {
        pid: 4242,
        what: "lab windows boot".to_owned(),
        checkout: String::new(),
        held: true,
        age: std::time::Duration::from_secs(3600),
        mem: 0,
        claimed: 0,
        alone: false,
        disk: 35 * GIB,
        quiet: false,
        order: (0, 4242, 0),
        stalled: Some(span),
        credit: Vec::new(),
        overrun: None,
    };
    let stuck = atpkg::admit_disk::Error::Stuck {
        stuck: aterm_toolchain::waits::Stuck {
            holders: vec![4242],
            span,
        },
        short: aterm_capacity::Shortfall {
            shorts: vec![aterm_capacity::Short::Disk {
                need: 70 * GIB,
                mount: PathBuf::from("/System/Volumes/Data"),
                above_floor: 90 * GIB,
                floor: 65 * GIB,
                reserved: 35 * GIB,
                writing: 35 * GIB,
                held: Vec::new(),
            }],
            holders: vec![holder],
        },
    };
    let why = answer(Err(stuck)).expect_err("could not run");
    for said in [
        "the run's disk claim gave up its wait",
        "made no progress for 16m00s (none was signalled)",
        "pid 4242 holds 35 GiB of disk",
        "which has made no progress for 16 min",
        "no check started",
    ] {
        assert!(why.contains(said), "{said:?} in {why}");
    }
    let unusable = atpkg::admit_disk::Error::Ledger(std::io::Error::other("read-only"));
    assert!(matches!(answer(Err(unusable)), Ok(Disk::Unclaimed)));
}

/// A disk claim more than the volume has free above its floor once its relief ran (UNMET) is
/// the run's COULD NOT RUN at once, naming the need, the volume, what holds the most there, that
/// it ends now rather than wait for disk and `--admit now`, and no space to free by hand.
/// NEGATIVE CONTROL: the same shortfall as a stuck wait's says that wait gave up, not that the
/// claim ended rather than wait for disk.
#[test]
fn a_disk_claim_no_wait_can_admit_could_not_run_naming_its_remedy() {
    let short = aterm_capacity::Shortfall {
        shorts: vec![aterm_capacity::Short::Disk {
            need: 68 * GIB,
            mount: PathBuf::from("/System/Volumes/Data"),
            above_floor: 24 * GIB,
            floor: 47 * GIB,
            reserved: 0,
            writing: 0,
            held: Vec::new(),
        }],
        holders: Vec::new(),
    };
    let unmet = atpkg::admit_disk::Error::Unmet {
        what: "verify lint test -p aterm-capacity".to_owned(),
        short: short.clone(),
        holders: Some("largest on /System/Volumes/Data: /Users//u/aterm-wt 30 GiB".to_owned()),
        relieved: true,
    };
    let why = answer(Err(unmet)).expect_err("could not run");
    for said in [
        "no check started",
        "verify lint test -p aterm-capacity",
        "needs 68 GiB on /System/Volumes/Data, 24 GiB free above its 47 GiB floor",
        "largest on /System/Volumes/Data: /Users//u/aterm-wt 30 GiB",
        "ends now rather than wait for disk",
        "--admit now",
    ] {
        assert!(why.contains(said), "{said:?} in {why}");
    }
    assert!(!why.contains("Free at least"), "{why}");
    let stuck = atpkg::admit_disk::Error::Stuck {
        stuck: aterm_toolchain::waits::Stuck {
            holders: vec![4242],
            span: std::time::Duration::from_secs(60),
        },
        short,
    };
    let why = answer(Err(stuck)).expect_err("could not run");
    assert!(!why.contains("ends now rather than wait for disk"), "{why}");
}

/// A run's footprints are kept by what it builds: its checks that build, sorted, and the
/// packages they read, sorted, so the order they were named in, and a check that only reads
/// the tree, keep the scope; the whole workspace, another package set, another check, or a
/// chosen cell is another scope.
#[test]
fn a_runs_scope_is_its_building_checks_and_packages() {
    let at = |scope: crate::Scope, checks: &[Check]| {
        scope_of(
            &Ctx::new(
                PathBuf::from("/w/aterm"),
                scope,
                crate::EnvSnapshot::default(),
                PathBuf::from("/tmp/x"),
            )
            .with_checks(checks),
        )
    };
    let p = |names: &[&str]| {
        crate::Scope::packages(&names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
    };
    let narrow = at(p(&["b", "a"]), &[Check::Test, Check::Lint]);
    assert_eq!(narrow, "lint test -p a -p b --no-incremental");
    assert_eq!(
        at(p(&["a", "b"]), &[Check::Fmt, Check::Lint, Check::Test]),
        narrow
    );
    assert_eq!(
        at(crate::Scope::workspace(), &[Check::Lint, Check::Test]),
        "lint test --workspace --no-incremental"
    );
    assert_ne!(at(p(&["a"]), &[Check::Lint, Check::Test]), narrow);
    assert_ne!(at(p(&["a", "b"]), &[Check::Lint]), narrow);
    // `-p` narrows `cells`, so its growth is no claim on the whole graph's run.
    assert_eq!(
        at(p(&["a"]), &[Check::Cells]),
        "cells -p a --no-incremental"
    );
    assert_eq!(
        at(crate::Scope::workspace(), &[Check::Cells]),
        "cells --no-incremental"
    );
    let cells = |chosen: Vec<String>| {
        scope_of(
            &Ctx::new(
                PathBuf::from("/w/aterm"),
                crate::Scope::workspace(),
                crate::EnvSnapshot::default(),
                PathBuf::from("/tmp/x"),
            )
            .with_checks(&[Check::Cells])
            .with_cells(chosen),
        )
    };
    assert_ne!(cells(vec!["win".to_owned()]), cells(Vec::new()));
    let run = run_of(
        &Ctx::new(
            PathBuf::from("/w/aterm"),
            p(&["a"]),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(&[Check::Lint]),
    )
    .unwrap()
    .expect("lint builds");
    assert_eq!(run.scope, "lint -p a --no-incremental");
}

/// A RUN'S SELECTION IN THE MACHINE'S RECORD is its building checks, sorted, each that builds
/// the whole tree whatever is selected marked `:tree`, what else changes their writes, and
/// the crates the selection names, sorted: the order they were named in and a check that only
/// reads the tree keep it; the whole workspace, or work no selection changes, names no crates;
/// `cells` under the change cone, which it does not read, is another work than under `-p`,
/// which it does.
#[test]
fn a_runs_selection_is_its_work_and_the_crates_it_did_it_for() {
    let at = |scope: crate::Scope, checks: &[Check]| {
        Ctx::new(
            PathBuf::from("/w/aterm"),
            scope,
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(checks)
    };
    let p = |names: &[&str]| {
        crate::Scope::packages(&names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
    };
    let sel = |work: &str, crates: Option<&[&str]>| atpkg::admit_disk::Selection {
        work: format!("{work} --no-incremental"),
        crates: crates.map(|c| c.iter().map(|n| (*n).to_owned()).collect()),
    };
    let narrow = selection_of(&at(p(&["b", "a"]), &[Check::Test, Check::Lint, Check::Fmt]));
    assert_eq!(narrow, sel("lint test", Some(&["a", "b"])));
    assert_eq!(
        selection_of(&at(p(&["a", "b"]), &[Check::Lint, Check::Test])),
        narrow
    );
    assert_eq!(
        selection_of(&at(crate::Scope::workspace(), &[Check::Lint, Check::Test])),
        sel("lint test", None)
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Cells, Check::Lint])),
        sel("cells lint", Some(&["a"]))
    );
    let cone = || crate::Scope::changed("origin/main", vec!["a".to_owned()], true);
    assert_eq!(
        selection_of(&at(cone(), &[Check::Cells, Check::Lint])),
        sel("cells:tree lint", Some(&["a"]))
    );
    assert_eq!(
        selection_of(&at(
            crate::Scope::changed("origin/main", Vec::new(), false),
            &[Check::Lint]
        )),
        sel("lint", Some(&[]))
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Appkit])),
        sel("appkit:tree", None)
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Test]).with_compile_only(true)),
        sel("--compile-only test", Some(&["a"]))
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Lint]).with_no_windows(true)),
        sel("lint", Some(&["a"])),
        "no check of it opens windows"
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Appkit]).with_no_windows(true)),
        sel("appkit--no-windows appkit:tree", None)
    );
    assert_eq!(
        selection_of(
            &at(crate::Scope::workspace(), &[Check::Cells])
                .with_cells(vec!["win".to_owned(), "linux".to_owned()])
        ),
        sel("cells--cell=linux,win cells:tree", None)
    );
    assert_eq!(
        selection_of(&at(cone(), &[Check::Cells, Check::Lint]).with_cells_pulled(true)),
        sel("cells--pulled cells:tree lint", Some(&["a"]))
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Test, Check::Lint]).with_checkers_in_tests(true)),
        sel("as-release lint test", Some(&["a"]))
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Lint]).with_checkers_in_tests(true)),
        sel("lint", Some(&["a"])),
        "lint builds no checkers"
    );
    // export-scan writes its export into the run's temporary directory, which no footprint
    // measures, so it is not in the work: a run with it and one without write the same.
    let dev = at(p(&["a"]), &crate::checks::DEV);
    assert_eq!(selection_of(&dev), sel("lint test", Some(&["a"])));
    assert_eq!(scope_of(&dev), "lint test -p a --no-incremental");
    assert!(
        run_of(&at(
            p(&["a"]),
            &[Check::Fmt, Check::Census, Check::ExportScan]
        ))
        .unwrap()
        .is_none(),
        "a run of checks that build nothing claims no disk"
    );
}

/// A run the machine's record keeps that did a run's work and more checks over its crates
/// bounds it: a fresh worktree's bare `verify -p a` claims what a kept `verify cells doc lint
/// test -p a -p b` wrote, not the cold budget. NEGATIVE CONTROL: a run whose test was narrowed
/// to compile-only supplies no cover by itself. Once wider work supplies the cover, the full
/// test still claims no less than that compile-only run grew: it necessarily does that work.
#[test]
fn a_kept_run_of_more_checks_bounds_a_run_of_fewer() {
    let at = |checks: &[Check]| {
        Ctx::new(
            PathBuf::from("/w/aterm"),
            crate::Scope::packages(&["a".to_owned()]),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(checks)
    };
    let asked = selection_of(&at(&crate::checks::DEV));
    let kept = |work: &str| atpkg::admit_disk::Selection {
        work: format!("{work} --no-incremental"),
        crates: Some(vec!["a".to_owned(), "b".to_owned()]),
    };
    let wrote = |gib: u64| atpkg::admit_disk::Footprint {
        peak: gib << 30,
        grew: gib << 30,
        at_ms: 1,
        dirs: Some(1),
        scope: None,
    };
    let mut m = aterm_capacity::MachineFootprints::default();
    m.push(kept("lint test--compile-only"), wrote(9));
    assert_eq!(
        m.cover(&asked, 0, COLD_BYTES),
        None,
        "a compile-only test did less"
    );
    m.push(kept("cells:tree lint test"), wrote(3));
    let cover = m
        .cover(&asked, 0, COLD_BYTES)
        .expect("the wider run covers it");
    assert_eq!(
        cover.bytes,
        9 << 30,
        "no less than its compile-only work grew"
    );
    assert!(cover.bounded, "the narrower run raises the wider figure");
}

/// EACH NARROWING WORD IS A RUN THAT DOES LESS, so a run with it bounds no run without it
/// ([`atpkg::admit_disk::Selection::does`]), and a run without it bounds one with it. `measure`
/// with no `--app` builds nothing (its measuring stages are named skips, its prebuild is not
/// planned), and with `--app` builds the measuring benches, so the absent app is the narrowing
/// (`measure--no-app`). `--compile-only` keeps the test compile alone of the whole plan, and
/// `--cursor-pacing` keeps the burst alone.
/// NEGATIVE CONTROLS, each the direction that would under-claim: a no-app `measure` run does
/// not bound one with `--app`; a compile-only `test` run does not bound a `test` run; a
/// cursor-pacing run does not bound a whole `measure` run.
#[test]
fn a_narrowing_word_is_a_run_that_does_less() {
    let at = |scope: crate::Scope, checks: &[Check]| {
        Ctx::new(
            PathBuf::from("/w/aterm"),
            scope,
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(checks)
    };
    let app = || Some(crate::MeasureArtifact::unnamed("/release/aterm.app"));
    let sel = |work: &str| atpkg::admit_disk::Selection {
        work: format!("{work} --no-incremental"),
        crates: None,
    };
    let whole = crate::Scope::workspace;

    let no_app = selection_of(&at(whole(), &[Check::Measure]));
    let with_app = selection_of(&at(whole(), &[Check::Measure]).with_measure_artifact(app()));
    assert_eq!(no_app, sel("measure--no-app measure:tree"));
    assert_eq!(with_app, sel("measure:tree"));
    assert!(!no_app.does(&with_app), "a run that built nothing");
    assert!(with_app.does(&no_app));

    let pacing = selection_of(
        &at(whole(), &[Check::Measure])
            .with_measure_artifact(app())
            .with_cursor_pacing(true),
    );
    assert_eq!(pacing, sel("--cursor-pacing measure:tree"));
    assert!(!pacing.does(&with_app));
    assert!(with_app.does(&pacing));

    let compiled = selection_of(&at(whole(), &[Check::Test]).with_compile_only(true));
    assert_eq!(compiled, sel("--compile-only test"));
    assert!(!compiled.does(&sel("test")), "it ran no test");
    let ran = selection_of(&at(whole(), &[Check::Test]));
    assert!(ran.does(&compiled), "a run of the tests did more");

    let windowless = selection_of(&at(whole(), &[Check::Appkit]).with_no_windows(true));
    let windowed = selection_of(&at(whole(), &[Check::Appkit]));
    assert!(!windowless.does(&windowed));
    assert!(windowed.does(&windowless));
}

/// A run measuring its workspace's intermediate root names the machine's record under this
/// user's aterm state directory (a test binary's own here), by its selection. NEGATIVE
/// CONTROL: with no selected measurable intermediate root, only `<target>/verify` is counted
/// and that incomplete observation is kept for no other checkout.
#[test]
fn a_run_names_the_machines_record_only_where_it_measures_its_build() {
    let mut ctx = Ctx::new(
        PathBuf::from("/w/aterm"),
        crate::Scope::packages(&["a".to_owned()]),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/x"),
    )
    .with_checks(&[Check::Lint]);
    ctx.main_build_dir = Some("build.build-dir='/c{workspace-root}/.cargo-build'".to_owned());
    let run = run_of(&ctx).unwrap().expect("a run that builds");
    assert_eq!(run.dirs[0], PathBuf::from("/c/w/aterm/.cargo-build"));
    let machine = run.machine.expect("kept machine-wide");
    let state = aterm_types::dirs::account_state_dir().expect("a test binary's state");
    assert_eq!(machine.record, state.join(MACHINE_FOOTPRINT));
    assert_eq!(machine.selection, selection_of(&ctx));
    ctx.main_build_dir = None;
    let run = run_of(&ctx).unwrap().expect("a run that builds");
    assert_eq!(run.dirs, vec![ctx.state_dir()]);
    assert_eq!(run.machine, None, "its build directory is not measured");
}

/// ONLY A RUN THAT PASSED IS KEPT FOR THIS MACHINE'S OTHER CHECKOUTS (`crate::let_disk_go`):
/// one that failed may have stopped short of what its scope writes, so it is kept beside its
/// directories alone, and each run's disk line says which records took it. NEGATIVE CONTROL:
/// the same run, passed, is in the machine's record, and its line says so.
#[cfg(unix)]
#[test]
fn only_a_run_that_passed_is_kept_for_this_machines_other_checkouts() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-disk-machine-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let record = dir.path().join("state").join(MACHINE_FOOTPRINT);
    let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
    let finished = |name: &str, passed: bool| -> String {
        let checkout = dir.path().join(name);
        let build = checkout.join("build");
        std::fs::create_dir_all(&build).expect("the build directory");
        let run = atpkg::admit_disk::Run {
            what: format!("verify lint -p a ({name})"),
            scope: "lint -p a".to_owned(),
            dirs: vec![build.clone()],
            record: checkout.join(FOOTPRINT),
            cold: 1 << 30,
            on: dir.path().to_path_buf(),
            stuck_ends: None,
            scratch: false,
            machine: Some(atpkg::admit_disk::Machinewide {
                record: record.clone(),
                selection: atpkg::admit_disk::Selection {
                    work: "lint".to_owned(),
                    crates: Some(vec!["a".to_owned()]),
                },
            }),
        };
        let held = atpkg::admit_disk::admit_on(
            run,
            &ledger,
            &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14)),
            aterm_capacity::Admit::Now,
            aterm_capacity::Pace::default(),
            &mut atpkg::admit_disk::Reclaim::of(
                Box::new(|_, _| (0, Vec::new(), Vec::new())),
                Box::new(|_, _, _| None),
                Box::new(|_, _| Vec::new()),
            ),
            &mut |_| {},
        )
        .expect("the run's disk claim");
        std::fs::write(build.join("unit.rlib"), [0u8; 1 << 16]).expect("the run's writes");
        let log_path = checkout.join("log");
        let mut log = std::fs::File::create(&log_path).expect("the log");
        crate::let_disk_go(Disk::Held(Box::new(held)), passed, Some(&mut log), None);
        assert_eq!(
            aterm_capacity::Footprints::load(&checkout.join(FOOTPRINT))
                .runs()
                .len(),
            1,
            "kept beside its directories"
        );
        std::fs::read_to_string(&log_path).expect("the log")
    };
    let kept_machine_wide = || {
        aterm_capacity::MachineFootprints::load(&record)
            .rows()
            .iter()
            .map(|r| r.runs.len())
            .sum::<usize>()
    };
    let failed = finished("failed", false);
    assert_eq!(kept_machine_wide(), 0, "{failed}");
    assert!(
        failed.contains(
            "; kept for its scope here; it did not pass, so not for this machine's other \
             checkouts"
        ),
        "{failed}"
    );
    let passed = finished("passed", true);
    assert_eq!(kept_machine_wide(), 1, "{passed}");
    assert!(
        passed.contains("; kept for its scope here; kept for this machine's other checkouts"),
        "{passed}"
    );
}

/// Set, the test binary is [`standin_a_runs_disk`], its run's directories under this one.
#[cfg(unix)]
const STANDIN: &str = "ATERM_VERIFY_DISK_STANDIN";
/// Set with [`STANDIN`], the stand-in holds a release's disk claim and hands its run its name.
#[cfg(unix)]
const COVERED: &str = "ATERM_VERIFY_DISK_STANDIN_COVERED";

/// The line [`standin_a_runs_disk`] says on stderr for the claim that covered its run: its
/// `what`, its name, and the name of the release's claim it holds, as JSON.
#[cfg(unix)]
const STANDIN_COVERED: &str = "standin-covered: ";

/// Run by [`a_covered_run_walks_nothing_and_keeps_nothing`] in a process of its own, whose walk
/// count ([`aterm_capacity::walks`]) nothing else moves: a verify run of `lint`, its build
/// directory and target under the stand-in's directory, its disk admitted ([`admit_in`]) in a
/// scratch ledger and let go as verify's run does ([`crate::let_disk_go`]), which says its
/// `verify-event` disk line on stderr. Where [`COVERED`] is set, it first holds a release's
/// disk claim and hands the run its name, as the release does, and says which claim covered
/// the run ([`STANDIN_COVERED`]).
#[cfg(unix)]
#[test]
fn standin_a_runs_disk() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    let mut room = aterm_capacity::Reading::of_gib(48, 40, 14);
    room.volume = Some(aterm_capacity::Volume {
        mount: dir.clone(),
        total: 400 * GIB,
        free: 300 * GIB,
        floor: 10 * GIB,
        room: 2 * GIB,
    });
    let machine = aterm_capacity::Fixed::new(room);
    let ledger = aterm_capacity::Ledger::at(dir.join("ledger"));
    let release = std::env::var_os(COVERED).map(|_| {
        atpkg::admit_disk::admit_sized_on(
            atpkg::admit_disk::SizedClaim {
                what: "release 0.156.0".to_owned(),
                bytes: 39 * GIB,
                on: dir.clone(),
                keep: Vec::new(),
                checkout: None,
                stuck_ends: None,
            },
            &ledger,
            &machine,
            atpkg::admit_disk::Admit::Now,
            aterm_capacity::Pace::default(),
            &mut atpkg::admit_disk::Reclaim::of(
                Box::new(|_, _| (0, Vec::new(), Vec::new())),
                Box::new(|_, _, _| None),
                Box::new(|_, _| Vec::new()),
            ),
            &mut |_| {},
        )
        .expect("the release's claim")
    });
    let build = dir.join("build");
    std::fs::create_dir_all(&build).expect("the build directory");
    std::fs::write(build.join("unit.rlib"), [0u8; 1 << 16]).expect("what it holds");
    let mut ctx = Ctx::new(
        dir.clone(),
        crate::Scope::packages(&["a".to_owned()]),
        crate::EnvSnapshot {
            cargo_target_dir: Some(dir.join("target")),
            disk_cover: release.as_ref().map(atpkg::admit_disk::Whole::marker),
            ..crate::EnvSnapshot::default()
        },
        dir.join("scratch"),
    )
    .with_checks(&[Check::Lint]);
    ctx.main_build_dir = Some(format!("build.build-dir='{}'", build.display()));
    let held = admit_in(
        &ctx,
        &ledger,
        &machine,
        std::process::id(),
        atpkg::admit_disk::Admit::Now,
    )
    .expect("the run's disk");
    if let Disk::Covered(by) = &held {
        eprintln!(
            "{STANDIN_COVERED}{}",
            aterm_json::json!({
                "what": by.need.what,
                "by": by.cover_marker(),
                "release": release.as_ref().map(atpkg::admit_disk::Whole::marker),
            })
        );
    }
    let mut log = std::fs::File::create(dir.join("log")).expect("the log");
    crate::let_disk_go(held, true, Some(&mut log), None);
    drop(release);
}

/// The stand-in run's `verify-event` disk line, the claim that covered it ([`STANDIN_COVERED`]),
/// and whether its record of what its scope wrote was written, run in `dir`, covered or not.
#[cfg(unix)]
fn a_runs_disk(dir: &Path, covered: bool) -> (aterm_json::Value, Option<aterm_json::Value>, bool) {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "disk::tests::standin_a_runs_disk",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(STANDIN, dir)
    .stdin(aterm_uds::spawnfd::Stdio::null());
    if covered {
        cmd.env(COVERED, "1");
    }
    let out = cmd.output().expect("the stand-in run");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    let event = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("verify-event: "))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .find(|v| v["event"].as_str() == Some("disk"))
        .unwrap_or_else(|| panic!("no disk event: {stderr}"));
    let by = stderr
        .lines()
        .filter_map(|l| l.strip_prefix(STANDIN_COVERED))
        .find_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok());
    let kept = dir.join("target/verify").join(FOOTPRINT).exists();
    (event, by, kept)
}

/// A RUN THE RELEASE'S DISK CLAIM COVERS WALKS NOTHING AND KEEPS NOTHING: handed the name of a
/// held disk claim of its own line, a verify run is covered by that claim (the release's, by
/// its name), makes no walk of its directories (none before it builds, none as it ends) and
/// writes no record of what its scope wrote, and its disk line says so. NEGATIVE CONTROL: the
/// same run handed nothing is covered by none, claims for itself, walks its directories as it
/// is admitted and once as it ends (it ran under the meter's pace), and keeps its run.
#[cfg(unix)]
#[test]
fn a_covered_run_walks_nothing_and_keeps_nothing() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-disk-covered-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let covered = dir.path().join("covered");
    std::fs::create_dir_all(&covered).expect("its directory");
    let (event, by, kept) = a_runs_disk(&covered, true);
    assert_eq!(
        (event["mode"].as_str(), event["walks"].as_u64()),
        (Some("covered"), Some(0)),
        "{event}"
    );
    let by = by.expect("the claim that covered it");
    assert_eq!(by["what"].as_str(), Some("release 0.156.0"), "{by}");
    assert!(
        by["by"].as_str().is_some() && by["by"] == by["release"],
        "covered by the release's claim: {by}"
    );
    assert!(!kept, "its record of what its scope wrote");

    let own = dir.path().join("own");
    std::fs::create_dir_all(&own).expect("its directory");
    let (event, by, kept) = a_runs_disk(&own, false);
    assert_eq!(
        (event["mode"].as_str(), event["walks"].as_u64()),
        (Some("claimed"), Some(2)),
        "{event}"
    );
    assert_eq!(by, None, "covered by none");
    assert!(kept, "its record of what its scope wrote");
}

/// Set, the test binary is [`standin_reads_the_disk_cover`].
#[cfg(unix)]
const CAPTURE: &str = "ATERM_VERIFY_DISK_COVER_CAPTURE";
/// What [`standin_reads_the_disk_cover`] says on stderr: the snapshot's cover, as JSON.
#[cfg(unix)]
const CAPTURED: &str = "captured-disk-cover: ";

/// Run by [`verify_reads_the_disk_cover_from_its_environment`] in a process of its own: the
/// disk cover the snapshot of its environment reads ([`crate::EnvSnapshot::capture`]).
#[cfg(unix)]
#[test]
fn standin_reads_the_disk_cover() {
    if std::env::var_os(CAPTURE).is_none() {
        return;
    }
    eprintln!(
        "{CAPTURED}{}",
        aterm_json::json!({ "cover": crate::EnvSnapshot::capture().disk_cover })
    );
}

/// VERIFY READS THE DISK COVER FROM ITS ENVIRONMENT ([`crate::EnvSnapshot::capture`]): a run
/// started with [`aterm_capacity::DISK_COVER_ENV`] set reads that marker as its cover, which is
/// how the release's verify runs are covered. NEGATIVE CONTROL: a run started with it blank,
/// or without it, reads none.
#[cfg(unix)]
#[test]
fn verify_reads_the_disk_cover_from_its_environment() {
    let read = |value: Option<&str>| {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args([
            "--exact",
            "disk::tests::standin_reads_the_disk_cover",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CAPTURE, "1")
        .env_remove(aterm_capacity::DISK_COVER_ENV)
        .stdin(aterm_uds::spawnfd::Stdio::null());
        if let Some(value) = value {
            cmd.env(aterm_capacity::DISK_COVER_ENV, value);
        }
        let out = cmd.output().expect("the stand-in run");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{stderr}");
        stderr
            .lines()
            .filter_map(|l| l.strip_prefix(CAPTURED))
            .find_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .unwrap_or_else(|| panic!("nothing captured: {stderr}"))["cover"]
            .as_str()
            .map(str::to_owned)
    };
    let marker = "0001793900000000-4242-7";
    assert_eq!(read(Some(marker)), Some(marker.to_owned()));
    assert_eq!(read(Some("  ")), None, "blank");
    assert_eq!(read(None), None, "unset");
}

/// THE COVER NEVER REACHES A CHILD: every child's environment loses the disk cover's name
/// ([`crate::CHILD_ENV_REMOVED`]), so a test binary a release's verify runs never reads it, and
/// the run reads it from its snapshot alone, even where its snapshot holds one.
#[test]
fn the_disk_cover_never_reaches_a_child() {
    let ctx = Ctx::new(
        PathBuf::from("/w/aterm"),
        crate::Scope::workspace(),
        crate::EnvSnapshot {
            disk_cover: Some("0001793900000000-4242-7".to_owned()),
            ..crate::EnvSnapshot::default()
        },
        PathBuf::from("/tmp/x"),
    );
    assert!(
        ctx.exec_env()
            .remove_env
            .contains(&aterm_capacity::DISK_COVER_ENV)
    );
    assert!(crate::CHILD_ENV_REMOVED.contains(&aterm_capacity::DISK_COVER_ENV));
}

/// A RUN OF ANY SCOPE WHOSE COMPILE CALLS COMPLETED KEEPS WHAT THEY USED at the top of its build
/// directory, as its scope's run (`atpkg::build_cache::stale::USED`, [`run_key`]): a dev run
/// over one package keeps each artifact's unit, fresh or built, by its profile, the packages it
/// built tests of, and when it began; a run over the whole workspace, one with `--as-release`
/// and one with `--compile-only` each keep a run of their own beside it and leave the others as
/// they were; and what a call into the lint lane reported ([`note_call`]) is kept at that lane's
/// top once the lane is closed, the main lane's record as it was. NEGATIVE CONTROLS: a lint
/// lane not yet closed keeps nothing; an artifact that is no test build names no package of the
/// workspace's own; a file cargo copied into the target with no twin in the build directory
/// names no unit; and a run whose closing call never passed, one a call of which failed, and one
/// that reported no unit keep nothing, before any record and beside one, whose bytes stay.
#[test]
fn a_run_of_any_scope_keeps_the_units_it_used() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-disk-used-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let leaf = dir.path().join("cache/w/.cargo-build");
    std::fs::create_dir_all(&leaf).expect("the build directory");
    let unit = |hash: &str| {
        leaf.join(format!(
            "debug/build/aterm-grid/{hash}/out/aterm_grid-{hash}"
        ))
        .display()
        .to_string()
    };
    let artifact = |filenames: Vec<String>, package: &str, test: bool| crate::shape::Artifact {
        filenames,
        fresh: test,
        package: Some(package.to_owned()),
        test,
    };
    let made = |scope: crate::Scope| {
        let mut ctx = Ctx::new(
            dir.path().join("w"),
            scope,
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(&[Check::Test]);
        ctx.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
        ctx
    };
    let note = |ctx: &Ctx, ok: bool| {
        let uplifted = ctx.target_dir.join("debug/aterm");
        note_used(
            ctx,
            &Dir::Main,
            &[
                artifact(vec![unit("1111111111111111")], "aterm-grid", true),
                artifact(
                    vec![unit("2222222222222222"), uplifted.display().to_string()],
                    "aterm",
                    false,
                ),
            ],
            &[],
            ok,
        );
    };
    let grid = || crate::Scope::packages(&["aterm-grid".to_owned()]);
    // Each a run that keeps nothing: its closing call never passed; a call of it failed; it
    // reported no unit.
    let keeps_nothing = || {
        let open = made(grid());
        note(&open, true);
        assert!(keep_used(&open).is_empty(), "its closing call never passed");
        let failed = made(grid());
        note(&failed, true);
        note(&failed, false);
        close(&failed, &Dir::Main);
        assert!(keep_used(&failed).is_empty(), "a call of it failed");
        let empty = made(grid());
        close(&empty, &Dir::Main);
        assert!(keep_used(&empty).is_empty(), "it reported no unit");
    };
    let record = leaf.join(atpkg::build_cache::stale::USED);
    keeps_nothing();
    assert!(!record.exists());

    let closed = |ctx: Ctx| {
        note(&ctx, true);
        close(&ctx, &Dir::Main);
        let said = keep_used(&ctx);
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(
            (&said[0].of, said[0].dir.as_path(), said[0].retired),
            (&Dir::Main, leaf.as_path(), 0),
            "{said:?}"
        );
        assert!(said[0].bound.is_some(), "{said:?}");
        ctx
    };
    let read = || {
        atpkg::build_cache::stale::Kept::decode(&std::fs::read(&record).expect("kept"))
            .expect("a record")
    };
    let ctx = closed(made(grid()));
    let first = read();
    assert_eq!(
        first.runs.keys().cloned().collect::<Vec<_>>(),
        vec![run_key(&ctx)]
    );
    let kept = &first.runs[&run_key(&ctx)];
    assert_eq!(
        kept.profiles
            .get("debug")
            .map(|h| h.iter().cloned().collect::<Vec<_>>()),
        Some(vec![
            "1111111111111111".to_owned(),
            "2222222222222222".to_owned()
        ])
    );
    assert_eq!(
        kept.members,
        std::collections::BTreeSet::from(["aterm-grid".to_owned()])
    );
    assert_eq!(
        Some(kept.began_ms),
        ctx.began
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
    );

    let wide = closed(made(crate::Scope::workspace()));
    let second = read();
    assert_eq!(second.runs.len(), 2, "{:?}", second.runs.keys());
    assert_eq!(
        second.runs[&run_key(&ctx)],
        first.runs[&run_key(&ctx)],
        "the narrower run stands as it was"
    );
    assert!(second.runs.contains_key(&run_key(&wide)));

    let release = closed(made(crate::Scope::workspace()).with_checkers_in_tests(true));
    let compile = closed(made(crate::Scope::workspace()).with_compile_only(true));
    let keys: std::collections::BTreeSet<String> =
        [&ctx, &wide, &release, &compile].map(run_key).into();
    assert_eq!(keys.len(), 4, "each a scope of its own: {keys:?}");
    assert_eq!(
        read()
            .runs
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        keys
    );

    let bytes = std::fs::read(&record).expect("kept");
    keeps_nothing();
    assert_eq!(std::fs::read(&record).expect("kept"), bytes);

    let mut lint = made(grid());
    lint.env.side_lanes_dir = Some(PathBuf::from("lanes"));
    let lane = crate::plan::lane_dir(&lint, crate::plan::Lane::TippyTarget).expect("a lane");
    std::fs::create_dir_all(&lane).expect("the lint lane");
    let checked = lane
        .join("debug/build/aterm-grid/5555555555555555/out/libaterm_grid-5555555555555555.rmeta");
    let wire = aterm_json::json!({
        "reason": "compiler-artifact",
        "package_id": "path+file:///w/aterm/crates/aterm-grid#0.155.0",
        "target": {"kind": ["lib"], "name": "aterm_grid"},
        "profile": {"test": true},
        "filenames": [checked.display().to_string()],
        "fresh": false,
    })
    .to_string();
    let call = crate::exec::Cmd::new("tippy").env("CARGO_TARGET_DIR", lane.clone());
    note_call(&lint, &call, Some(&wire), true);
    assert!(keep_used(&lint).is_empty(), "the lint lane is not closed");
    close(&lint, &Dir::Side(lane.clone()));
    let said = keep_used(&lint);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(
        (&said[0].of, said[0].dir.as_path(), said[0].retired),
        (&Dir::Side(lane.clone()), lane.as_path(), 0),
        "{said:?}"
    );
    let side = atpkg::build_cache::stale::Kept::decode(
        &std::fs::read(lane.join(atpkg::build_cache::stale::USED)).expect("kept"),
    )
    .expect("a record");
    let side = &side.runs[&run_key(&lint)];
    assert_eq!(
        side.profiles["debug"],
        std::collections::BTreeSet::from(["5555555555555555".to_owned()])
    );
    assert_eq!(
        side.members,
        std::collections::BTreeSet::from(["aterm-grid".to_owned()])
    );
    assert_eq!(
        std::fs::read(&record).expect("kept"),
        bytes,
        "the main lane's record is not the lint lane's"
    );
}

/// THE RUN'S END TRIMS WHAT ITS RECORD RETIRED ([`trim_used`], `atpkg::build_cache::trim_run_end`):
/// a checkout's first run keeps the two test units it used and retires nothing, so nothing is
/// surveyed or taken (a third unit of their kind that no run used and nothing wrote for five days
/// stands); its second, of the same scope, used that third in place of one of them, so the record
/// retires the one and the trim takes it whole, an hour old and inside the stale window, says
/// the bytes that went, and leaves cargo's lock, the record, no stage, and every unit the run
/// used, each unwritten for five days, so the record alone keeps them; a third run alike retires
/// nothing and takes nothing; and a stage a trim left in the build directory is finished by the
/// next run to end there, though it retires nothing. NEGATIVE CONTROLS: while a build holds the
/// profile's cargo lock the retired unit stands and the run says so; a retired unit written
/// since the run that retired it began stands; and a build directory replaced under the run,
/// another directory standing at its path, is not read.
#[cfg(unix)]
#[test]
fn the_run_end_trim_takes_the_retired_bytes_and_leaves_every_unit_the_run_used() {
    use atpkg::build_cache::{Trimmed, Unused};
    const HOUR: std::time::Duration = std::time::Duration::from_secs(60 * 60);
    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    const OLD: &str = "1111111111111111";
    const NEW: &str = "2222222222222222";
    const BOTH: &str = "3333333333333333";
    const LATER: &str = "4444444444444444";
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-disk-trim-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let leaf = dir.path().join("cache/w/.cargo-build");
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).expect("the profile");
    let lock = profile.join(".cargo-build-lock");
    std::fs::write(&lock, b"").expect("cargo's lock");
    let folder = |hash: &str| profile.join("build/aterm-grid").join(hash);
    let out = |hash: &str| folder(hash).join(format!("out/aterm_grid-{hash}"));
    let dated = |hash: &str, at: std::time::SystemTime| {
        let unit = folder(hash);
        for path in [
            unit.join("fingerprint/test-lib-aterm_grid"),
            unit.join("fingerprint/test-lib-aterm_grid.json"),
            unit.join("fingerprint"),
            out(hash),
            unit.join("out"),
            unit,
        ] {
            std::fs::File::open(path)
                .and_then(|f| f.set_modified(at))
                .expect("dated");
        }
    };
    let lay = |hash: &str, bytes: usize, ago: std::time::Duration| {
        let unit = folder(hash);
        std::fs::create_dir_all(unit.join("fingerprint")).expect("its fingerprint");
        std::fs::create_dir_all(unit.join("out")).expect("its outputs");
        std::fs::write(unit.join("fingerprint/test-lib-aterm_grid"), b"h").expect("hash");
        std::fs::write(
            unit.join("fingerprint/test-lib-aterm_grid.json"),
            b"{\"rustc\":2,\"features\":\"[]\"}",
        )
        .expect("fingerprint");
        std::fs::write(out(hash), vec![7u8; bytes]).expect("its test");
        dated(hash, std::time::SystemTime::now() - ago);
    };
    lay(OLD, 64 << 10, HOUR);
    lay(NEW, 1 << 10, 5 * DAY);
    lay(BOTH, 1 << 10, 5 * DAY);
    let ran = |hashes: [&str; 2]| {
        let mut ctx = Ctx::new(
            dir.path().join("w"),
            crate::Scope::packages(&["aterm-grid".to_owned()]),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(&[Check::Test]);
        ctx.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
        let artifacts = hashes.map(|hash| crate::shape::Artifact {
            filenames: vec![out(hash).display().to_string()],
            fresh: true,
            package: Some("aterm-grid".to_owned()),
            test: true,
        });
        note_used(&ctx, &Dir::Main, &artifacts, &[], true);
        close(&ctx, &Dir::Main);
        let kept = keep_used(&ctx);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].dir, leaf, "{kept:?}");
        assert!(kept[0].bound.is_some(), "{kept:?}");
        (ctx, kept)
    };
    // Each call of the cache's entry and what it took; the probe is inert: no fixture reads
    // the person's processes.
    let calls = std::cell::RefCell::new(Vec::<Trimmed>::new());
    let trim = |ctx: &Ctx, kept: &[KeptDir]| {
        trim_used_with(ctx, kept, &|dir| {
            let trimmed = atpkg::build_cache::trim_run_end_with_processes(
                dir,
                std::time::SystemTime::now(),
                &|| true,
                &|_, _| Ok(aterm_uds::holders::PathScan::default()),
            );
            calls.borrow_mut().push(trimmed.clone());
            trimmed
        })
    };
    let last = || calls.borrow().last().cloned().expect("the trim ran");
    let took = |trimmed: &Trimmed| -> usize {
        trimmed
            .went
            .iter()
            .map(|(_, why)| match why {
                Unused::Stale { superseded, .. } => *superseded,
                _ => 0,
            })
            .sum()
    };
    let stages = || {
        std::fs::read_dir(&leaf)
            .expect("the build directory")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cargo-build.removing")
            })
            .count()
    };

    let (ctx, kept) = ran([OLD, BOTH]);
    assert_eq!(kept[0].retired, 0, "{kept:?}");
    assert_eq!(trim(&ctx, &kept), Vec::<String>::new());
    assert_eq!(
        last(),
        Trimmed::default(),
        "nothing retired and no stage left: nothing is surveyed or taken"
    );
    assert!(
        out(NEW).is_file(),
        "a unit no run used waits for a retirement"
    );

    let (ctx, kept) = ran([NEW, BOTH]);
    assert_eq!(kept[0].retired, 1, "{kept:?}");
    let held = aterm_toolchain::record_lock::RecordLock::try_lock(
        std::fs::File::open(&lock).expect("cargo's lock"),
        0,
    )
    .expect("held");
    let said = trim(&ctx, &kept);
    assert_eq!(said.len(), 1, "{said:?}");
    let refused = last();
    assert!(
        refused.went.is_empty() && refused.freed == 0 && refused.kept.is_some(),
        "{refused:?}"
    );
    assert!(
        out(OLD).is_file() && stages() == 0,
        "a build holds the lock"
    );
    drop(held);

    let said = trim(&ctx, &kept);
    assert_eq!(said.len(), 1, "{said:?}");
    let trimmed = last();
    assert_eq!(took(&trimmed), 1, "{trimmed:?}");
    assert!(
        trimmed.freed >= 64 << 10 && trimmed.kept.is_none(),
        "{trimmed:?}"
    );
    assert!(
        said[0].contains(&atpkg::admit_disk::size(trimmed.freed)),
        "the line says the bytes that went: {said:?}"
    );
    assert!(!folder(OLD).exists() && stages() == 0);
    for hash in [NEW, BOTH] {
        assert!(
            out(hash).is_file()
                && folder(hash)
                    .join("fingerprint/test-lib-aterm_grid")
                    .is_file(),
            "{hash}"
        );
    }
    assert!(lock.is_file());
    let read = atpkg::build_cache::stale::Kept::decode(
        &std::fs::read(leaf.join(atpkg::build_cache::stale::USED)).expect("kept"),
    )
    .expect("a record");
    assert_eq!(
        read.runs[&run_key(&ctx)].profiles["debug"],
        std::collections::BTreeSet::from([NEW.to_owned(), BOTH.to_owned()])
    );

    let (ctx, kept) = ran([NEW, BOTH]);
    assert_eq!(kept[0].retired, 0, "{kept:?}");
    assert_eq!(trim(&ctx, &kept), Vec::<String>::new());
    assert_eq!(last(), Trimmed::default(), "an identical run takes nothing");

    // A trim that was stopped once it had moved its units aside left its stage in the build
    // directory, where no pass of the cache looks: the next run to end there finishes it.
    let left = leaf.join(".cargo-build.removing-2");
    std::fs::create_dir_all(left.join("0")).expect("a stage left");
    std::fs::write(left.join("0/part"), vec![7u8; 8 << 10]).expect("what it holds");
    let (ctx, kept) = ran([NEW, BOTH]);
    assert_eq!(kept[0].retired, 0, "{kept:?}");
    let said = trim(&ctx, &kept);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(last().went, vec![(left.clone(), Unused::Unfinished)]);
    assert!(!left.exists() && stages() == 0, "{said:?}");

    // NEGATIVE CONTROL: the run retires `NEW`, which something wrote after the run began.
    lay(LATER, 1 << 10, HOUR);
    let (ctx, kept) = ran([LATER, BOTH]);
    assert_eq!(kept[0].retired, 1, "{kept:?}");
    dated(NEW, std::time::SystemTime::now() + HOUR);
    let said = trim(&ctx, &kept);
    assert_eq!(said.len(), 1, "{said:?}");
    let refused = last();
    assert!(
        refused.went.is_empty() && refused.kept.is_some(),
        "{refused:?}"
    );
    assert!(out(NEW).is_file() && out(LATER).is_file() && out(BOTH).is_file() && stages() == 0);

    // NEGATIVE CONTROL: the build directory the record was written in is moved away, and
    // another stands at its path when the trim comes to it.
    let (ctx, kept) = ran([NEW, BOTH]);
    assert_eq!(kept[0].retired, 1, "{kept:?}");
    let aside = dir.path().join("cache/w/aside");
    std::fs::rename(&leaf, &aside).expect("moved away");
    std::fs::create_dir_all(&leaf).expect("another directory at its path");
    let before = calls.borrow().len();
    let said = trim(&ctx, &kept);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(
        calls.borrow().len(),
        before,
        "a directory the record was not written in is not read"
    );
    assert!(
        aside
            .join("debug/build/aterm-grid")
            .join(LATER)
            .join("fingerprint")
            .is_dir(),
        "the retired unit stands where its directory went"
    );
}

/// A CELL'S DIRECTORY IS A BUILD DIRECTORY OF ITS OWN ([`dir_of`], `crate::cells::cell_dir`): a
/// call whose `CARGO_TARGET_DIR` names the directory of one of forge's cells in the cells lane
/// builds into a side lane of that path, whether the lanes stand beside the run's target or in
/// the shared cache; the main lane's and the lint lane's stay theirs. NEGATIVE CONTROLS: the
/// cells lane itself, each folder in it that is no cell's (the browser smoke's, the wasm glue's,
/// the browser frames', the windows-sys oracles'), a folder inside a cell's directory, and a
/// cell's directory where the run's lanes are not, name none; and a call into one of those notes
/// nothing and keeps nothing, though it reported a unit of a member and its directory was closed.
#[test]
fn a_cells_directory_is_a_build_directory_of_its_own() {
    let made = |lanes: Option<PathBuf>| {
        let mut ctx = Ctx::new(
            PathBuf::from("/repo"),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        );
        ctx.cached_lanes_dir = lanes;
        ctx
    };
    let into = |dir: &Path| crate::exec::Cmd::new("cargo").env("CARGO_TARGET_DIR", dir);
    let beside = made(None);
    let cached = made(Some(PathBuf::from("/cache/repo/.cargo-build/verify/.ubd")));
    for cell in aterm_forge::resolve::default_cells() {
        let own = crate::cells::cell_dir(&beside, &cell.name);
        let moved = crate::cells::cell_dir(&cached, &cell.name);
        assert_ne!(own, moved, "{}", cell.name);
        assert_eq!(
            dir_of(&beside, &into(&own)),
            Some(Dir::Side(own.clone())),
            "{}",
            cell.name
        );
        assert_eq!(
            dir_of(&cached, &into(&moved)),
            Some(Dir::Side(moved.clone())),
            "{}",
            cell.name
        );
        assert_eq!(
            dir_of(&cached, &into(&own)),
            None,
            "{}: the run's lanes are not there",
            cell.name
        );
    }
    let lane = crate::plan::cells_dir(&beside);
    let elsewhere = [
        lane.clone(),
        lane.join("wasm-smoke"),
        lane.join("wasm-glue"),
        lane.join("browser"),
        lane.join("win32-oracle-bin"),
        crate::cells::cell_dir(&beside, "linux-arm").join("aarch64-unknown-linux-gnu"),
    ];
    for dir in &elsewhere {
        assert_eq!(dir_of(&beside, &into(dir)), None, "{}", dir.display());
    }
    // CONTROLS: no variable and the run's target are the main lane's, and the lint lane's path
    // is that lane's.
    assert_eq!(
        dir_of(&beside, &crate::exec::Cmd::new("cargo")),
        Some(Dir::Main)
    );
    assert_eq!(dir_of(&beside, &into(&beside.target_dir)), Some(Dir::Main));
    let lint =
        crate::plan::lane_dir(&beside, crate::plan::Lane::TippyTarget).expect("the lint lane");
    assert_eq!(dir_of(&beside, &into(&lint)), Some(Dir::Side(lint.clone())));

    let smoke = &elsewhere[1];
    let unit = smoke
        .join("wasm32-unknown-unknown/debug/deps/libaterm_wasm-5555555555555555.rmeta")
        .display()
        .to_string();
    let wire = aterm_json::json!({
        "reason": "compiler-artifact",
        "package_id": "path+file:///repo/crates/aterm-wasm#0.155.0",
        "target": {"kind": ["lib"], "name": "aterm_wasm"},
        "profile": {"test": false},
        "filenames": [unit],
        "fresh": true,
    })
    .to_string();
    let members = std::collections::BTreeSet::from(["aterm-wasm".to_owned()]);
    note_call_owning(&beside, &into(smoke), Some(&wire), true, &members);
    assert!(
        beside
            .shape_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .used
            .is_empty(),
        "a call into a folder that is no cell's notes nothing"
    );
    close(&beside, &Dir::Side(smoke.clone()));
    assert!(keep_used(&beside).is_empty(), "and keeps nothing");
}

/// A RUN THAT BUILT A SECOND HASH RECORDS WHAT MOVED ([`note_used`], [`record_second_hashes`]): a
/// test compile builds `aterm-grid`'s test unit under a second hash beside its first, which differs
/// only in its dependency `mid`'s hash, and `mid`'s two builds only in theirs of `aterm-json`
/// (named `json` in `mid`'s `deps`), whose features moved; the run's end says it in one line of
/// its log, one `second-hash` event and one row of the machine's record, which names
/// `aterm-json`'s `features`, and counts it by cause. NEGATIVE CONTROLS: the cause is the
/// dependency, not `mid` between; the first unit of its kind (`solo`) records nothing; a unit
/// cargo found fresh is not read; the same unit noted again is not read again, and a run that
/// built no second hash records nothing. A bin a test needs, which the compile names by its copy
/// in the target alone, is read as the unit its copy came from; and two units of one kind the
/// run built both of, each found beside the other, are one second hash, said once.
#[test]
fn a_run_that_built_a_second_hash_records_what_moved() {
    const GRID_OLD: &str = "a1a1a1a1a1a1a1a1";
    const GRID_NEW: &str = "a2a2a2a2a2a2a2a2";
    const SOLO: &str = "d1d1d1d1d1d1d1d1";
    const TOOL_OLD: &str = "e1e1e1e1e1e1e1e1";
    const TOOL_NEW: &str = "e2e2e2e2e2e2e2e2";
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-disk-second-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let leaf = dir.path().join("cache/w/.cargo-build");
    let profile = leaf.join("debug");
    let lay =
        |package: &str, hash: &str, kind: &str, fp: u64, features: &str, deps: &[(&str, u64)]| {
            let unit = profile.join("build").join(package).join(hash);
            std::fs::create_dir_all(unit.join("fingerprint")).expect("its fingerprint");
            std::fs::create_dir_all(unit.join("out")).expect("its outputs");
            let hex: String = fp
                .to_le_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            std::fs::write(unit.join("fingerprint").join(kind), hex).expect("its hash file");
            let json = aterm_json::json!({
                "rustc": 1, "features": features, "declared_features": "[]", "target": 2,
                "profile": 3, "path": 4,
                "deps": deps.iter().map(|(name, h)| aterm_json::json!([5, *name, false, *h]))
                    .collect::<Vec<_>>(),
                "local": [{"CheckDepInfo": {"dep_info": format!("debug/build/{package}/{hash}/x"),
                    "fingerprint": "mtime"}}],
                "rustflags": [], "config": 6, "compile_kind": 0,
            });
            std::fs::write(
                unit.join("fingerprint").join(format!("{kind}.json")),
                aterm_json::to_string(&json).expect("json"),
            )
            .expect("its fingerprint JSON");
            let out = unit.join("out").join(format!("{package}-{hash}"));
            std::fs::write(&out, b"o").expect("its output");
            out.display().to_string()
        };
    let json_old = lay(
        "aterm-json",
        "c1c1c1c1c1c1c1c1",
        "lib-aterm_json",
        501,
        "[]",
        &[],
    );
    let json_new = lay(
        "aterm-json",
        "c2c2c2c2c2c2c2c2",
        "lib-aterm_json",
        502,
        "[\"std\"]",
        &[],
    );
    lay(
        "mid",
        "b1b1b1b1b1b1b1b1",
        "lib-mid",
        601,
        "[]",
        &[("json", 501)],
    );
    lay(
        "mid",
        "b2b2b2b2b2b2b2b2",
        "lib-mid",
        602,
        "[]",
        &[("json", 502)],
    );
    let old = lay(
        "aterm-grid",
        GRID_OLD,
        "test-lib-aterm_grid",
        701,
        "[]",
        &[("mid", 601)],
    );
    let new = lay(
        "aterm-grid",
        GRID_NEW,
        "test-lib-aterm_grid",
        702,
        "[]",
        &[("mid", 602)],
    );
    let solo = lay("solo", SOLO, "lib-solo", 801, "[]", &[]);
    // A bin of two builds whose features moved; cargo copied the new one's into the target.
    lay("tool", TOOL_OLD, "bin-tool", 901, "[]", &[]);
    lay("tool", TOOL_NEW, "bin-tool", 902, "[\"cli\"]", &[]);
    for (hash, bytes) in [(TOOL_OLD, &b"the old bin"[..]), (TOOL_NEW, &b"new"[..])] {
        std::fs::write(
            profile.join("build/tool").join(hash).join("out/tool"),
            bytes,
        )
        .expect("its bin");
    }
    let events = dir.path().join("run.events");
    let mut ctx = Ctx::new(
        dir.path().join("w"),
        crate::Scope::packages(&["aterm-grid".to_owned()]),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/x"),
    )
    .with_checks(&[Check::Test])
    .with_events(Some(
        crate::event_log::EventLog::open(&events).expect("events"),
    ));
    ctx.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
    let bin = ctx.target_dir.join("debug/tool");
    std::fs::create_dir_all(ctx.target_dir.join("debug")).expect("the target");
    std::fs::hard_link(
        profile.join("build/tool").join(TOOL_NEW).join("out/tool"),
        &bin,
    )
    .expect("cargo's copy of the bin");
    let artifact = |file: &str, package: &str, fresh: bool| crate::shape::Artifact {
        filenames: vec![file.to_owned()],
        fresh,
        package: Some(package.to_owned()),
        test: true,
    };
    let record = dir.path().join("state/verify/second-hashes.json");
    // Cargo found the old unit fresh, and `solo` is the first of its kind: nothing to record.
    note_used(
        &ctx,
        &Dir::Main,
        &[
            artifact(&old, "aterm-grid", true),
            artifact(&solo, "solo", false),
        ],
        &[],
        true,
    );
    assert!(record_second_hashes(&ctx, Some(&record)).is_empty());
    assert!(
        !record.exists(),
        "a run that built no second hash records nothing"
    );

    note_used(
        &ctx,
        &Dir::Main,
        &[
            artifact(&new, "aterm-grid", false),
            // Both builds of `aterm-json`, each beside the other.
            artifact(&json_old, "aterm-json", false),
            artifact(&json_new, "aterm-json", false),
            crate::shape::Artifact {
                filenames: vec![bin.display().to_string()],
                fresh: false,
                package: Some("tool".to_owned()),
                test: false,
            },
        ],
        &[],
        true,
    );
    // Noted again by a later call: read once.
    note_used(
        &ctx,
        &Dir::Main,
        &[artifact(&new, "aterm-grid", false)],
        &[],
        true,
    );
    let said = record_second_hashes(&ctx, Some(&record));
    // One line for each second hash, and one for what the record kept.
    assert_eq!(said.len(), 4, "{said:?}");
    let grid = said
        .iter()
        .find(|l| l.contains(GRID_NEW))
        .expect("aterm-grid's line");
    for fact in [GRID_OLD, "aterm-json", "features"] {
        assert!(grid.contains(fact), "{fact} in {grid}");
    }
    assert_eq!(
        said.iter()
            .filter(|l| l.contains("c1c1c1c1c1c1c1c1"))
            .count(),
        1,
        "two builds of one kind, each beside the other, said once: {said:?}"
    );
    assert!(
        said.iter()
            .any(|l| l.contains(TOOL_NEW) && l.contains(TOOL_OLD)),
        "the bin, read by the unit its copy came from: {said:?}"
    );
    assert!(
        said.iter()
            .any(|l| l.contains(&record.display().to_string())),
        "{said:?}"
    );
    let rows = aterm_capacity::second_hash::SecondHashes::load(&record);
    let rows = rows.rows();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let row = |package: &str| {
        rows.iter()
            .find(|r| r.package == package)
            .unwrap_or_else(|| panic!("{package}'s row in {rows:?}"))
    };
    assert_eq!(
        (
            row("aterm-grid").hash.as_str(),
            row("aterm-grid").other.as_str()
        ),
        (GRID_NEW, GRID_OLD)
    );
    assert_eq!(
        (
            row("aterm-grid").cause.as_str(),
            row("aterm-grid").input.as_str()
        ),
        ("aterm-json", "features"),
        "the dependency whose own input moved, not `mid` between"
    );
    assert_eq!(
        (
            row("tool").hash.as_str(),
            row("tool").other.as_str(),
            row("tool").input.as_str()
        ),
        (TOOL_NEW, TOOL_OLD, "features")
    );
    let text = std::fs::read_to_string(&events).expect("the events");
    let events: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("\"second-hash\""))
        .collect();
    assert_eq!(events.len(), 3, "{events:?}");
    let event = events
        .iter()
        .find(|l| l.contains(&format!("\"hash\":\"{GRID_NEW}\"")))
        .expect("aterm-grid's second-hash event");
    for field in ["\"cause\":\"aterm-json\"", "\"input\":\"features\""] {
        assert!(event.contains(field), "{field} in {event}");
    }
    let (inputs, causes) = aterm_capacity::second_hash::SecondHashes::load(&record).by_cause();
    assert_eq!(inputs.get("features"), Some(&3), "{inputs:?}");
    assert_eq!(
        causes.get(&("aterm-json".to_owned(), "features".to_owned())),
        Some(&2),
        "{causes:?}"
    );
    assert!(
        record_second_hashes(&ctx, Some(&record)).is_empty(),
        "each second hash is recorded once"
    );
}

/// WHERE `targo`'S RECEIPTS NAME THE UNITS A RUN USED, THEY ARE ITS RECORD ([`keep_used`],
/// [`receipts_speak`], DC-12d): a run of one scope whose build directory holds no receipt keeps
/// its run as before; the scope's next run, whose builds' receipts (begun since it began) name
/// every unit it used, keeps no run of its own: the record takes the receipts, its scope's run
/// before goes, and the unit only that run used is retired for the trim, with the directory
/// bound, and the log says so. NEGATIVE CONTROLS: a receipt that names only some of the run's
/// units, one of a build begun before the run began, and receipts that name them all beside one
/// the read left out (torn), leave the run to keep itself, as before receipts.
#[test]
fn a_run_whose_builds_receipts_name_its_units_keeps_no_run_of_its_own() {
    use atpkg::build_cache::stale;
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-disk-receipts-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let leaf = dir.path().join("cache/w/.cargo-build");
    std::fs::create_dir_all(&leaf).expect("the build directory");
    let grid = || crate::Scope::packages(&["aterm-grid".to_owned()]);
    let made = || {
        let mut ctx = Ctx::new(
            dir.path().join("w"),
            grid(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(&[Check::Test]);
        ctx.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
        ctx
    };
    let run = |ctx: &Ctx, hashes: &[&str]| {
        let artifacts: Vec<crate::shape::Artifact> = hashes
            .iter()
            .map(|hash| crate::shape::Artifact {
                filenames: vec![
                    leaf.join(format!(
                        "debug/build/aterm-grid/{hash}/out/aterm_grid-{hash}"
                    ))
                    .display()
                    .to_string(),
                ],
                fresh: true,
                package: Some("aterm-grid".to_owned()),
                test: true,
            })
            .collect();
        note_used(ctx, &Dir::Main, &artifacts, &[], true);
        close(ctx, &Dir::Main);
        keep_used(ctx)
    };
    let millis = |at: std::time::SystemTime| {
        u64::try_from(
            at.duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_millis(),
        )
        .expect("ms")
    };
    let receipt = |began_ms: u64, hashes: &[&str]| {
        let folder = leaf.join(stale::RECEIPTS);
        std::fs::create_dir_all(&folder).expect("the receipts");
        let named: Vec<String> = hashes.iter().map(|h| format!("\"{h}\"")).collect();
        let file = folder.join("00000000000000a1.json");
        std::fs::write(
            &file,
            format!(
                "{{\"began_ms\":{began_ms},\"members\":[\"aterm-grid\"],\
                 \"profiles\":{{\"debug\":[{}]}},\"receipt\":1,\"selection\":{{}}}}",
                named.join(",")
            ),
        )
        .expect("a receipt");
        // Dated by the clock the run's start was read from: the file system dates a write by its
        // own coarser one (Windows' tick of about 15.6 ms, a Linux jiffy), which puts this write,
        // made within that step of the run's start, before it, and a receipt is read as begun no
        // later than it was written. A real build's receipt is written long after its run began.
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()))
            .expect("the receipt dated");
    };
    let read = || stale::Kept::read(&leaf);

    let first = made();
    let said = run(&first, &["1111111111111111", "3333333333333333"]);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].line.starts_with("disk: kept the 2 unit(s)"),
        "{said:?}"
    );
    let key = run_key(&first);
    assert!(read().runs.contains_key(&key));

    let partial = made();
    receipt(millis(partial.began), &["1111111111111111"]);
    let said = run(&partial, &["1111111111111111", "2222222222222222"]);
    assert!(
        said[0].line.starts_with("disk: kept the 2 unit(s)"),
        "{said:?}"
    );
    assert!(
        read().runs.contains_key(&key),
        "a receipt naming some of its units"
    );

    let earlier = made();
    receipt(
        millis(earlier.began) - 1000,
        &["1111111111111111", "2222222222222222"],
    );
    let said = run(&earlier, &["1111111111111111", "2222222222222222"]);
    assert!(
        said[0].line.starts_with("disk: kept the 2 unit(s)"),
        "{said:?}"
    );
    assert!(
        read().runs.contains_key(&key),
        "a receipt of a build begun before the run"
    );

    let unread = made();
    receipt(
        millis(unread.began),
        &["1111111111111111", "2222222222222222"],
    );
    let torn = leaf.join(stale::RECEIPTS).join("00000000000000b2.json");
    std::fs::write(&torn, b"{\"began_ms\":").expect("a torn receipt");
    let said = run(&unread, &["1111111111111111", "2222222222222222"]);
    assert!(
        said[0].line.starts_with("disk: kept the 2 unit(s)"),
        "{said:?}"
    );
    assert!(
        read().runs.contains_key(&key),
        "a receipt the read left out"
    );
    std::fs::remove_file(&torn).expect("the torn receipt removed");

    let spoken = made();
    // The scope's run before used 1111 and 2222 (the `unread` run's): this run's builds used
    // 1111 and 4444.
    receipt(
        millis(spoken.began),
        &["1111111111111111", "4444444444444444"],
    );
    let said = run(&spoken, &["1111111111111111", "4444444444444444"]);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].line.contains("receipts of this run's builds")
            && said[0]
                .line
                .contains("1 unit(s) nothing kept names retired"),
        "{said:?}"
    );
    assert_eq!(
        (said[0].retired, said[0].bound.is_some()),
        (1, true),
        "{said:?}"
    );
    let kept = read();
    assert!(
        !kept.runs.contains_key(&key),
        "the receipts speak for the scope"
    );
    assert_eq!(kept.receipts.len(), 1);
    assert_eq!(
        kept.retired.profiles["debug"],
        std::collections::BTreeSet::from(["2222222222222222".to_owned()])
    );
}
