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
    .with_checks(&[Check::Lint, Check::Test, Check::Doc]);
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
/// the run's COULD NOT RUN at once, naming the need, how much to free on which volume, what
/// holds the most there and `--admit now`. NEGATIVE CONTROL: the same shortfall as a stuck
/// wait's says that wait gave up, not what to free.
#[test]
fn a_disk_claim_no_wait_can_admit_could_not_run_naming_its_remedy() {
    let short = aterm_capacity::Shortfall {
        shorts: vec![aterm_capacity::Short::Disk {
            need: 68 * GIB,
            mount: PathBuf::from("/System/Volumes/Data"),
            above_floor: 24 * GIB,
            floor: 47 * GIB,
            reserved: 0,
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
        "Free at least 44 GiB on /System/Volumes/Data",
        "--admit now",
    ] {
        assert!(why.contains(said), "{said:?} in {why}");
    }
    let stuck = atpkg::admit_disk::Error::Stuck {
        stuck: aterm_toolchain::waits::Stuck {
            holders: vec![4242],
            span: std::time::Duration::from_secs(60),
        },
        short,
    };
    let why = answer(Err(stuck)).expect_err("could not run");
    assert!(!why.contains("Free at least"), "{why}");
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
        selection_of(
            &at(p(&["a"]), &[Check::Test, Check::Doc, Check::Lint]).with_checkers_in_tests(true)
        ),
        sel("as-release doc lint test", Some(&["a"]))
    );
    assert_eq!(
        selection_of(&at(p(&["a"]), &[Check::Lint]).with_checkers_in_tests(true)),
        sel("lint", Some(&["a"])),
        "lint builds no checkers"
    );
    // export-scan writes its export into the run's temporary directory, which no footprint
    // measures, so it is not in the work: a run with it and one without write the same.
    let dev = at(p(&["a"]), &crate::checks::DEV);
    assert_eq!(selection_of(&dev), sel("doc lint test", Some(&["a"])));
    assert_eq!(scope_of(&dev), "doc lint test -p a --no-incremental");
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
    m.push(kept("doc lint test--compile-only"), wrote(9));
    assert_eq!(
        m.cover(&asked, 0, COLD_BYTES),
        None,
        "a compile-only test did less"
    );
    m.push(kept("cells:tree doc lint test"), wrote(3));
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
/// (`measure--no-app`). `--compile-only` keeps the test compile alone of the whole plan, so it
/// narrows a pulled `wasm-smoke` too, and `--cursor-pacing` keeps the burst alone.
/// NEGATIVE CONTROLS, each the direction that would under-claim: a no-app `measure` run does
/// not bound one with `--app`; a compile-only `test` run that pulled `wasm-smoke` (which then
/// did not run) does not bound a `wasm-smoke` run; a cursor-pacing run does not bound a whole
/// `measure` run.
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

    let compiled =
        selection_of(&at(whole(), &[Check::Test, Check::WasmSmoke]).with_compile_only(true));
    assert_eq!(compiled, sel("--compile-only test wasm-smoke:tree"));
    assert!(
        !compiled.does(&sel("wasm-smoke:tree")),
        "it ran no wasm smoke"
    );
    assert!(!compiled.does(&sel("test wasm-smoke:tree")));
    assert!(compiled.does(&sel("--compile-only test")));
    let ran = selection_of(&at(whole(), &[Check::Test, Check::WasmSmoke]));
    assert!(ran.does(&compiled), "a run of both did more");

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
const STANDIN: &str = "ATERM_VERIFY_DISK_STANDIN";
/// Set with [`STANDIN`], the stand-in holds a release's disk claim and hands its run its name.
const COVERED: &str = "ATERM_VERIFY_DISK_STANDIN_COVERED";

/// The line [`standin_a_runs_disk`] says on stderr for the claim that covered its run: its
/// `what`, its name, and the name of the release's claim it holds, as JSON.
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
const CAPTURE: &str = "ATERM_VERIFY_DISK_COVER_CAPTURE";
/// What [`standin_reads_the_disk_cover`] says on stderr: the snapshot's cover, as JSON.
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

/// A RELEASE RUN OVER THE WHOLE WORKSPACE THAT PASSED KEEPS WHAT ITS TEST COMPILE USED at the
/// top of its build directory (`atpkg::build_cache::stale::USED`): each artifact's unit, fresh
/// or built, by its profile; the packages it built tests of; and when it began. NEGATIVE
/// CONTROLS: an artifact that is no test build names no package of the workspace's own; a file
/// cargo copied into the target with no twin in the build directory names no unit; and the same
/// run keeps nothing where it is no release's, failed, or is over less than the workspace.
#[test]
fn a_passing_release_run_keeps_the_units_it_used() {
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
    let noted = |scope: crate::Scope| {
        let mut ctx = Ctx::new(
            dir.path().join("w"),
            scope,
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/x"),
        )
        .with_checks(&[Check::Test]);
        ctx.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
        let uplifted = ctx.target_dir.join("debug/aterm");
        note_used(
            &ctx,
            &[
                artifact(vec![unit("1111111111111111")], "aterm-grid", true),
                artifact(
                    vec![unit("2222222222222222"), uplifted.display().to_string()],
                    "aterm",
                    false,
                ),
            ],
            &[],
        );
        ctx
    };
    let record = leaf.join(atpkg::build_cache::stale::USED);
    assert_eq!(
        keep_used(&noted(crate::Scope::workspace()), true),
        None,
        "no release's run"
    );
    let release = |scope| noted(scope).with_checkers_in_tests(true);
    assert_eq!(
        keep_used(&release(crate::Scope::workspace()), false),
        None,
        "a run that failed"
    );
    assert_eq!(
        keep_used(
            &release(crate::Scope::packages(&["aterm-grid".to_owned()])),
            true
        ),
        None,
        "a run over less than the workspace"
    );
    assert!(!record.exists());
    let ctx = release(crate::Scope::workspace());
    assert!(keep_used(&ctx, true).is_some(), "kept");
    let kept = atpkg::build_cache::stale::Used::decode(&std::fs::read(&record).expect("kept"))
        .expect("a record");
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
}
