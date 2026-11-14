// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::EnvSnapshot;
use crate::checks::{CHECKS, Check, DEV, RELEASE};
use std::path::PathBuf;

pub(super) fn ctx(checks: &[Check], scope: Scope) -> Ctx {
    Ctx::new(
        PathBuf::from("/repo"),
        scope,
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(checks)
}

fn ids(ctx: &Ctx) -> Vec<StageId> {
    plan(ctx).into_iter().map(|s| s.id).collect()
}

/// The run `verify <words>` plans, built from its parse as the CLI builds it.
fn parsed(words: &[&str]) -> Ctx {
    let a = crate::cli::parse(words.iter().copied()).expect("parses");
    let checks = a.wanted(|p| Scope::Workspace.includes_crate(p));
    let app = a
        .app
        .as_ref()
        .map(|app| crate::MeasureArtifact::unnamed(app.clone()));
    ctx(&checks, Scope::workspace())
        .with_release(a.release)
        .with_checkers_in_tests(a.release || a.as_release)
        .with_measure_artifact(app)
}

/// B3's plan identity: `verify release` plans the stages of `release --tree` and of
/// `release --bundle` together, but the measuring benches' own compile, which its test compile
/// builds, as the tree half's does. The tree half plans no timed stage and judges no bundle.
/// Negative control: a tree half short of one check (spec) leaves its stage out of the union.
#[test]
fn a_release_plans_its_tree_half_and_its_bundle_half_together() {
    use std::collections::BTreeSet;
    let set = |c: &Ctx| {
        ids(c)
            .into_iter()
            .map(|id| id as u8)
            .collect::<BTreeSet<u8>>()
    };
    let release = parsed(&["release", "--app", "/release/aterm.app"]);
    let tree = parsed(&["release", "--tree"]);
    let bundle = parsed(&["release", "--bundle", "--app", "/release/aterm.app"]);
    assert!(folds_measuring(&release) && folds_measuring(&tree));
    assert!(!folds_measuring(&bundle) && ids(&bundle).contains(&StageId::MeasurePrebuild));
    assert!(ids(&tree).into_iter().all(|id| !id.timed()));
    let union = |tree: &Ctx| {
        let mut halves: BTreeSet<u8> = set(tree).union(&set(&bundle)).copied().collect();
        halves.remove(&(StageId::MeasurePrebuild as u8));
        halves
    };
    assert_eq!(set(&release), union(&tree));
    assert_eq!(
        ids(&parsed(&[
            "release",
            "--tree",
            "--bundle",
            "--app",
            "/release/aterm.app"
        ])),
        ids(&release)
    );

    let short = parsed(&["release", "--checks", "fmt,lint,test,doc,census,forge,libc"]);
    assert_ne!(
        set(&release),
        union(&short),
        "spec's stage is in neither half"
    );
}

/// A repeatable tree spends its independently admitted stage grants one at a time even
/// though it has no outer build slot. NEGATIVE CONTROL: the same ordinary plan retains
/// parallel lanes.
#[test]
fn a_background_plan_is_exclusive_without_an_outer_slot() {
    let ordinary = ctx(&DEV, Scope::workspace());
    assert!(plan(&ordinary).iter().any(|stage| !stage.exclusive));
    let mut background = ctx(&DEV, Scope::workspace());
    background.background = true;
    assert!(plan(&background).iter().all(|stage| stage.exclusive));
}

fn every_check() -> Vec<Check> {
    CHECKS.iter().map(|s| s.check).collect()
}

#[test]
fn every_side_lane_is_a_folder_spotlight_skips() {
    for (env, target) in [
        (
            EnvSnapshot::default(),
            PathBuf::from("/repo/target/.ubd/main.noindex"),
        ),
        (
            EnvSnapshot {
                cargo_target_dir: Some(PathBuf::from("/fast/t")),
                ..EnvSnapshot::default()
            },
            PathBuf::from("/fast/t"),
        ),
    ] {
        let c = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            env,
            PathBuf::from("/tmp"),
        );
        let lane = |l| lane_dir(&c, l).expect("a directory");
        for (l, name) in [
            (Lane::TippyTarget, "tippy.noindex"),
            (Lane::CellsTarget, "cells.noindex"),
        ] {
            let quiet = target.join("verify").join(".ubd").join(name);
            assert_eq!(lane(l), quiet, "{l:?}");
            assert!(aterm_tempfile::is_quiet(&quiet), "{quiet:?}");
        }
        assert_eq!(
            lane(Lane::LibcOracleTarget),
            PathBuf::from("/repo/libc-oracle/.ubd/target.noindex")
        );
        assert_eq!(lane(Lane::MainTarget), target);
        assert_eq!(
            aterm_tempfile::is_quiet(&target),
            c.env.cargo_target_dir.is_none()
        );
        assert_eq!(lane_dir(&c, Lane::Pure), None);
    }
}

/// The trust lane's old per-checkout and shared-cache places are both retired, while the two
/// live side lanes keep their shared-cache directories. NEGATIVE CONTROL: deriving every
/// retired path from [`SIDE_LANES`] either misses trust or removes a live lane.
#[cfg(unix)]
#[test]
fn the_retired_trust_lane_is_removed_without_retiring_a_live_lane() {
    let c = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    );
    let asked = std::sync::Mutex::new(Vec::new());
    assert!(
        retire_old_lanes_with(&c, &|path| {
            asked.lock().expect("paths").push(path.to_path_buf());
            Ok(false)
        })
        .is_empty()
    );
    let asked = asked.into_inner().expect("paths");
    let state = c.state_dir();
    let lanes = lanes_dir(&c);
    assert_eq!(
        asked,
        [
            state.join("tippy.noindex"),
            state.join("trust.noindex"),
            state.join("cells.noindex"),
            lanes.join("trust.noindex"),
        ]
    );
    for live in SIDE_LANES {
        assert!(
            !asked.contains(&lanes.join(noindex(live))),
            "the live {live} lane must retain its cache"
        );
    }
}

/// WHAT AN EARLIER RUN SET ASIDE OF AN OLD LANE IS TRIED AGAIN, THE LANE GONE OR NOT: an aside
/// of the old tippy lane keeping one entry its owner flagged `uchg` (the remover never clears
/// a flag) loses the rest and is said, naming it, though no `tippy.noindex` is left; once the
/// flag is off, the next run removes it and says nothing. NEGATIVE CONTROL: the flag refuses
/// `std::fs::remove_file`.
#[cfg(target_os = "macos")]
#[test]
fn an_old_lanes_stuck_aside_is_tried_again_once_the_lane_is_gone() {
    /// Clears the flag when dropped, so a failed test leaves its scratch removable.
    struct Unflag(PathBuf);
    impl Drop for Unflag {
        fn drop(&mut self) {
            let _ = atpkg::platform::set_file_flags(&self.0, 0);
        }
    }
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let target = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("target");
    let c = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot {
            cargo_target_dir: Some(target),
            ..EnvSnapshot::default()
        },
        PathBuf::from("/tmp"),
    );
    let aside = c.state_dir().join("tippy.noindex.removing");
    let stuck = aside.join("debug/stuck");
    std::fs::create_dir_all(stuck.parent().expect("its folder")).expect("a folder");
    std::fs::write(&stuck, "x").expect("written");
    std::fs::write(aside.join("debug/free"), "x").expect("written");
    let _unflag = Unflag(stuck.clone());
    atpkg::platform::set_file_flags(&stuck, libc::UF_IMMUTABLE).expect("uchg");
    assert!(std::fs::remove_file(&stuck).is_err(), "the flag refuses it");
    assert!(
        c.state_dir()
            .join("tippy.noindex")
            .symlink_metadata()
            .is_err()
    );

    let retire = || {
        retire_old_lanes_with(&c, &|path| {
            atpkg::build_cache::remove_unbuilt_with_processes(path, &|_, _| {
                Ok(aterm_uds::holders::PathScan::default())
            })
        })
    };
    let said = retire();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains(&*aside.to_string_lossy()), "{said:?}");
    assert!(stuck.is_file() && !aside.join("debug/free").exists());
    atpkg::platform::set_file_flags(&stuck, 0).expect("the flag off");
    assert_eq!(retire(), Vec::<String>::new());
    assert!(aside.symlink_metadata().is_err(), "the aside went");
}

#[test]
fn compile_only_uses_the_normal_test_compile_without_running_tests() {
    let normal = ctx(&[Check::Test], Scope::Workspace);
    let expected = plan(&normal)
        .into_iter()
        .find(|s| s.id == StageId::TestCompile)
        .unwrap();
    let focused = normal.with_compile_only(true);
    let planned = plan(&focused);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].id, expected.id);
    assert_eq!(planned[0].mem, expected.mem);
    assert_eq!(
        focused.rerun(Check::Test),
        "targo --unverified verify test --all --compile-only"
    );
    assert!(
        plan(&ctx(&[Check::Test], Scope::Workspace))
            .iter()
            .any(|s| s.id == StageId::Test)
    );
}

#[test]
fn compile_only_measure_builds_its_matrix_without_a_product_or_timing_admission() {
    let parsed = crate::cli::parse(["measure", "--compile-only"]).unwrap();
    assert!(parsed.compile_only);
    assert!(crate::cli::parse(["release", "--app", "a", "--compile-only"]).is_err());
    assert!(
        crate::cli::parse(["measure", "--app", "a", "--compile-only", "--cursor-pacing"]).is_err()
    );
    let prepare = ctx(&[Check::Measure], Scope::Workspace).with_compile_only(true);
    let planned = plan(&prepare);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].id, StageId::MeasurePrebuild);
    assert!(!planned[0].id.timed());
    assert!(prepare.rerun(Check::Measure).ends_with(" --compile-only"));
    // NEGATIVE CONTROL: the normal command must still execute the matrices and GUI smoke.
    let normal = ctx(&[Check::Measure], Scope::Workspace)
        .with_measure_artifact(Some(crate::MeasureArtifact::unnamed("/a/aterm.app")));
    assert!(
        plan(&normal)
            .iter()
            .any(|s| s.id == StageId::MeasuringTests)
    );
    assert!(plan(&normal).iter().any(|s| s.id == StageId::GuiSmoke));
    assert_ne!(
        crate::disk::selection_of(&prepare),
        crate::disk::selection_of(&normal)
    );
    assert_eq!(
        crate::disk::scope_of(&prepare),
        crate::disk::scope_of(&normal)
    );
}

/// A release's red `test` or `doc` row prints a command the CLI takes and that runs the check
/// as the release did: without `--as-release` it enables no checkers and owes no golden test,
/// so what failed there would pass. CONTROL: a plain run's and another check's name no flag.
#[test]
fn a_releases_red_test_row_reruns_as_the_release_ran() {
    let release = ctx(&[Check::Test, Check::Doc, Check::Lint], Scope::Workspace).with_release(true);
    for check in [Check::Test, Check::Doc] {
        let printed = release.rerun(check);
        assert_eq!(
            printed,
            format!(
                "targo --unverified verify {} --all --as-release",
                check.name()
            )
        );
        let words = printed
            .strip_prefix("targo --unverified verify ")
            .expect("the verb")
            .split(' ');
        let parsed = crate::cli::parse(words).expect("the CLI takes what the row prints");
        assert!(parsed.as_release && parsed.all && parsed.checks == [check]);
        assert_eq!(
            parsed.wanted(|p| Scope::Workspace.includes_crate(p)),
            [check],
            "no release runs what `test` would pull"
        );
    }
    let without = crate::cli::parse(["test", "--all"]).expect("parses");
    assert_eq!(
        without.wanted(|p| Scope::Workspace.includes_crate(p)),
        [Check::Test, Check::WasmSmoke],
        "a plain `test --all` still pulls it"
    );
    let compiled =
        crate::cli::parse(["test", "--all", "--compile-only", "--as-release"]).expect("parses");
    assert_eq!(
        compiled.wanted(|p| Scope::Workspace.includes_crate(p)),
        [Check::Test]
    );
    assert!(!release.rerun(Check::Lint).contains("--as-release"));
    let plain = ctx(&[Check::Test], Scope::Workspace);
    assert_eq!(
        plain.rerun(Check::Test),
        "targo --unverified verify test --all"
    );
    assert_eq!(
        plain
            .with_checkers_in_tests(true)
            .with_compile_only(true)
            .rerun(Check::Test),
        "targo --unverified verify test --all --compile-only --as-release"
    );
}

/// A cone's red `lint` row reruns the cone by its base, which runs the same passes: `-p` over
/// its crates would lint every target of each, and the crates the cone's lint left out.
/// CONTROL: `test` and `doc` take every crate of the cone, so theirs name them; `-p` and the
/// workspace rerun as they ran.
#[test]
fn a_cones_red_lint_row_reruns_the_cone_by_its_base() {
    let cone = Scope::changed(
        "origin/main",
        vec!["dev".to_string(), "seed".to_string()],
        true,
    )
    .with_all_targets(vec!["seed".to_string()])
    .with_unlinted(vec!["dev".to_string()]);
    let plain = ctx(&[Check::Lint, Check::Test, Check::Doc], cone);
    let printed = plain.rerun(Check::Lint);
    assert_eq!(printed, "targo --unverified verify lint --base origin/main");
    let words = printed
        .strip_prefix("targo --unverified verify ")
        .expect("the verb")
        .split(' ');
    let parsed = crate::cli::parse(words).expect("the CLI takes what the row prints");
    assert_eq!(parsed.checks, [Check::Lint]);
    assert_eq!(parsed.base.as_deref(), Some("origin/main"));
    assert!(parsed.packages.is_empty() && !parsed.all && parsed.narrows());
    for (check, name) in [(Check::Test, "test"), (Check::Doc, "doc")] {
        assert_eq!(
            plain.rerun(check),
            format!("targo --unverified verify {name} -p dev -p seed")
        );
    }
    assert_eq!(
        ctx(&[Check::Lint], Scope::crate_only("seed")).rerun(Check::Lint),
        "targo --unverified verify lint -p seed"
    );
    assert_eq!(
        ctx(&[Check::Lint], Scope::Workspace).rerun(Check::Lint),
        "targo --unverified verify lint --all"
    );
}

#[test]
fn compile_only_keeps_the_shared_test_graph_and_neither_runtime() {
    let full = ctx(&[Check::Test, Check::Checkers], Scope::Workspace);
    let expected: Vec<_> = plan(&full)
        .into_iter()
        .filter(|s| s.id == StageId::TestCompile)
        .map(|s| (s.id, s.mem, s.lane))
        .collect();
    let focused = full.with_compile_only(true);
    let actual: Vec<_> = plan(&focused)
        .into_iter()
        .map(|s| (s.id, s.mem, s.lane))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 1);
    assert!(focused.rerun(Check::Checkers).ends_with(" --compile-only"));
    assert!(
        plan(&ctx(&[Check::Test, Check::Checkers], Scope::Workspace))
            .iter()
            .any(|s| s.id == StageId::SpecCheckers)
    );
}

/// A run that measures memory plans each stage's claim from its book: the test compile its
/// kept peak and margin, the lint with no retained row the cold figure, the test run (whose
/// dispatcher claims each schedule's concurrent peak) and the formatter (which touches no
/// target directory) nothing.
/// NEGATIVE CONTROL: a run that measures nothing plans no claim for any stage.
#[test]
fn a_run_that_measures_memory_plans_each_stages_claim_from_its_book() {
    const GIB: u64 = 1 << 30;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let book = crate::memory::Book::at(
        Some(dir.path().join("stage-memory.json")),
        Some(24 * GIB),
        Some(43 * GIB / 2),
    );
    let scope = Scope::crate_only("aterm-gui");
    let compile = declared(&scope)
        .into_iter()
        .find(|s| s.id == StageId::TestCompile)
        .expect("declared");
    let checks = [Check::Test, Check::Lint, Check::Fmt];
    let key = crate::memory::Key::of_run(compile.id, &ctx(&checks, scope.clone()));
    book.keep(&compile, &key, 10 * GIB).expect("kept");
    let claims =
        |c: &Ctx| -> Vec<(StageId, u64)> { plan(c).into_iter().map(|s| (s.id, s.mem)).collect() };
    assert_eq!(
        claims(&ctx(&checks, scope.clone()).with_memory(book)),
        [
            (StageId::Formatting, 0),
            (StageId::Tippy, 12 * GIB),
            (StageId::TestCompile, 12 * GIB),
            // The dispatch claims its plan's peak; the stage must not claim it again.
            (StageId::Test, 0),
        ]
    );
    assert!(
        claims(&ctx(&checks, scope))
            .iter()
            .all(|(_, mem)| *mem == 0)
    );
}

/// A HEAVY TEST COMPILE IS PLANNED AS BOUNDED CALLS, IN ITS PLACE: the same measured S1 graph
/// plans a package selection and the workspace as two main-lane stages, each below the live
/// shaping budget, before the test run. The closing call keeps the run's own selection and
/// the doctests await both calls. NEGATIVE CONTROLS: a run that measures nothing plans one
/// whole call; restoring the workspace exclusion does too.
#[test]
fn a_heavy_test_compile_is_planned_as_its_calls_in_its_place() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("stage-memory.json");
    let mut records = crate::shape::tests::book_at_1023();
    records.push(
        crate::shape::unit_key(
            "/fixture",
            &crate::unitgraph::UnitName {
                package: "aterm-gui".to_owned(),
                kind: "aterm_gui test".to_owned(),
            },
        ),
        1553u64 * (1u64 << 30) / 100,
        1,
    );
    records.save(&path).expect("saved");
    fn members(_: &Ctx) -> crate::changed::Members {
        crate::changed::Members::new(
            crate::shape::tests::s1()
                .into_iter()
                .map(|name| crate::changed::Member {
                    dir: format!("crates/{name}"),
                    name,
                    has_lib: true,
                })
                .collect(),
        )
    }
    let checks = [Check::Test, Check::Doc];
    for scope in [
        Scope::packages(&crate::shape::tests::s1()),
        Scope::workspace(),
    ] {
        let book = crate::memory::Book::at(Some(path.clone()), Some(24 << 30), Some(24 << 30))
            .with_shape_budget(Some(crate::shape::tests::BUDGET));
        let mut c = ctx(&checks, scope.clone()).with_memory(book);
        c.unit_graph = crate::shape::tests::fixture_graph;
        c.workspace_members = members;
        let planned = plan(&c);
        let compiles: Vec<(usize, &StageSpec)> = planned
            .iter()
            .enumerate()
            .filter(|(_, s)| s.id == StageId::TestCompile)
            .collect();
        assert_eq!(compiles.len(), 2, "{}", scope.label());
        assert!(
            compiles.iter().all(|(_, s)| {
                s.lane == Lane::MainTarget && s.mem < crate::shape::tests::BUDGET
            })
        );
        assert!(
            compiles[0]
                .1
                .part
                .as_ref()
                .is_some_and(|p| p.shaped() && !p.closing())
        );
        assert!(
            compiles[1]
                .1
                .part
                .as_ref()
                .is_some_and(crate::shape::Part::closing)
        );
        assert!(compiles[1].1.title.contains(&scope.label()));
        let compile_indices = compiles.iter().map(|(i, _)| *i).collect::<Vec<_>>();
        let test = planned
            .iter()
            .position(|s| s.id == StageId::Test)
            .expect("test stage");
        assert!(
            compile_indices.iter().all(|compile| *compile < test),
            "the same-lane test runs serially after every compile call"
        );
        let doctests = planned
            .iter()
            .position(|s| s.id == StageId::Doctests)
            .expect("doctest stage");
        assert_eq!(
            crate::sched::awaited(&planned, doctests).collect::<Vec<_>>(),
            compile_indices,
            "doctests must await every shaped call"
        );
        crate::sched::check_waits(&planned).expect("each awaits only earlier stages");

        let whole = plan(&ctx(&checks, scope));
        assert_eq!(
            whole
                .iter()
                .filter(|s| s.id == StageId::TestCompile)
                .count(),
            1
        );
        assert!(whole.iter().all(|s| s.part.is_none()));
    }
}

/// AN UNSHAPED TEST COMPILE CLAIMS AT LEAST ITS ESTIMATE: `-p aterm-gui -p aterm-lab`, whose
/// only kept figure is the `-p aterm-gui` it holds (17.63 GiB in aterm-vmem), is not shaped
/// (21.16 GiB, under the 21.3 GiB budget) and claims that, not the cold 12 GiB its stage's
/// lookup found; `-p aterm-gui`, whose record here is a fresh run's 32 MiB, claims by
/// aterm-vmem's 17.63 GiB and not 1.03 GiB, and its time line says so. NEGATIVE CONTROL, by
/// hand: with the estimate dropped, they claimed 12 GiB and 1.03 GiB.
#[test]
fn an_unshaped_test_compile_claims_at_least_its_estimate() {
    const GIB: u64 = 1 << 30;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("stage-memory.json");
    let key = |checkout: &str, crates: &[&str]| crate::memory::Key {
        stage: "TestCompile".to_owned(),
        checkout: checkout.to_owned(),
        crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
        variant: None,
    };
    let mut r = crate::memory::Records::default();
    r.push(key("/vmem", &["aterm-gui"]), 1763 * GIB / 100, 1);
    r.save(&path).expect("saved");
    let claim = |crates: &[&str]| -> (u64, Option<String>) {
        let book = crate::memory::Book::at(
            Some(path.clone()),
            Some(24 * GIB),
            Some(crate::shape::tests::BUDGET),
        );
        let crates: Vec<String> = crates.iter().map(|c| (*c).to_owned()).collect();
        let c = ctx(&[Check::Test], Scope::packages(&crates)).with_memory(book);
        let p = plan(&c);
        let s = p
            .iter()
            .find(|s| s.id == StageId::TestCompile)
            .expect("planned");
        (s.mem, s.part.as_ref().map(|p| p.said.clone()))
    };
    let gui = aterm_capacity::peak_claim(1763 * GIB / 100);
    assert!(gui < crate::shape::tests::BUDGET);

    let (mem, said) = claim(&["aterm-gui", "aterm-lab"]);
    assert_eq!(mem, gui);
    assert!(
        said.as_deref()
            .is_some_and(|s| s.contains("the narrower selections it holds, summed")
                && s.contains("more than the 12 GiB its stage's lookup found")),
        "{said:?}"
    );

    r.push(key("/repo", &["aterm-gui"]), 32 << 20, 2);
    r.save(&path).expect("saved");
    let (mem, said) = claim(&["aterm-gui"]);
    assert_eq!(mem, gui);
    assert!(
        said.as_deref()
            .is_some_and(|s| s.contains("the most `-p aterm-gui` kept in /vmem")),
        "{said:?}"
    );
}

/// THE TEST COMPILE CLAIMS AT LEAST ITS ESTIMATE, THE LINT THE LESSER: `-p a -p b` in /repo,
/// with `-p a` kept at 3 GiB in /one and `-p b` at 4 GiB in /two (a cover of 7 GiB) and `-p a
/// -p b -p c` at 5 GiB in /far, for the test compile and the lint alike. The lint's lookup
/// claims the lesser of the wider record elsewhere and the cover, 6 GiB
/// ([`crate::memory::wider_elsewhere`]); the test compile's lookup found the same 6 GiB, but
/// its estimate is the narrower selections' heaviest cover, so it claims that, 8.4 GiB, and its
/// time line says so. NEGATIVE CONTROL, by hand: with the estimate dropped ([`at_least`] not
/// called), the test compile claimed 6 GiB.
#[test]
fn the_test_compile_claims_its_estimate_where_the_lint_claims_the_lesser() {
    const GIB: u64 = 1 << 30;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("stage-memory.json");
    let mut r = crate::memory::Records::default();
    for stage in ["TestCompile", "Tippy"] {
        let key = |checkout: &str, crates: &[&str]| crate::memory::Key {
            stage: stage.to_owned(),
            checkout: checkout.to_owned(),
            crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
            variant: None,
        };
        r.push(key("/one", &["a"]), 3 * GIB, 1);
        r.push(key("/two", &["b"]), 4 * GIB, 2);
        r.push(key("/far", &["a", "b", "c"]), 5 * GIB, 3);
    }
    r.save(&path).expect("saved");
    let book = crate::memory::Book::at(
        Some(path),
        Some(48 * GIB),
        Some(crate::shape::tests::BUDGET),
    );
    let crates = ["a".to_owned(), "b".to_owned()];
    let c = ctx(&[Check::Test, Check::Lint], Scope::packages(&crates)).with_memory(book);
    let p = plan(&c);
    let of = |id: StageId| {
        let s = p.iter().find(|s| s.id == id).expect("planned");
        (s.mem, s.part.as_ref().map(|p| p.said.clone()))
    };
    assert_eq!(
        of(StageId::Tippy),
        (aterm_capacity::peak_claim(5 * GIB), None)
    );
    let (mem, said) = of(StageId::TestCompile);
    assert_eq!(mem, aterm_capacity::peak_claim(7 * GIB));
    assert!(
        said.as_deref().is_some_and(|s| s.contains(&format!(
            "more than the {} its stage's lookup found",
            aterm_capacity::size(aterm_capacity::peak_claim(5 * GIB))
        ))),
        "{said:?}"
    );
}

/// A named skip plans no memory: under `--no-windows` the redraw conformance and the objc
/// drives, which would compile and so claim the cold figure, are skipped before they start
/// anything and claim nothing. NEGATIVE CONTROL: the same run with windows plans the cold 12
/// GiB of a 24 GiB Mac for each.
#[test]
fn a_named_skip_plans_no_memory() {
    const GIB: u64 = 1 << 30;
    let book = || crate::memory::Book::at(None, Some(24 * GIB), Some(43 * GIB / 2));
    let claims = |no_windows: bool| -> Vec<(StageId, u64)> {
        let c = ctx(&[Check::Appkit], Scope::workspace())
            .with_no_windows(no_windows)
            .with_memory(book());
        plan(&c).into_iter().map(|s| (s.id, s.mem)).collect()
    };
    assert_eq!(
        claims(true),
        [(StageId::RedrawConformance, 0), (StageId::ObjcDrives, 0)]
    );
    assert_eq!(
        claims(false),
        [
            (StageId::RedrawConformance, 12 * GIB),
            (StageId::ObjcDrives, 12 * GIB)
        ]
    );
}

/// An empty change cone (a change to documents alone) plans no memory and no compile for the
/// stages that run over the selection: each only says the cone holds no crate, so none waits
/// for a claim it never uses (a changelog's lint row waited nine minutes for 24.5 GiB).
/// NEGATIVE CONTROL: a cone of one crate plans the cold figure for the doctests and the lint,
/// and compiles.
#[test]
fn an_empty_cone_plans_no_memory_for_the_stages_over_its_selection() {
    const GIB: u64 = 1 << 30;
    let run = |crates: Vec<String>| {
        ctx(
            &[Check::Doc, Check::Lint, Check::Fmt],
            Scope::changed("origin/main", crates, true),
        )
        .with_memory(crate::memory::Book::at(
            None,
            Some(24 * GIB),
            Some(43 * GIB / 2),
        ))
    };
    let claims =
        |c: &Ctx| -> Vec<(StageId, u64)> { plan(c).into_iter().map(|s| (s.id, s.mem)).collect() };
    let empty = run(Vec::new());
    assert_eq!(
        claims(&empty),
        [
            (StageId::Formatting, 0),
            (StageId::Tippy, 0),
            (StageId::Doctests, 0),
        ]
    );
    assert!(!has_compiler_stage(&empty));
    let one = run(vec!["seed".to_string()]);
    let over: Vec<(StageId, u64)> = claims(&one)
        .into_iter()
        .filter(|(id, _)| id.over_selection())
        .collect();
    assert!(
        over.len() == 2 && over.iter().all(|(_, mem)| *mem > 0),
        "{over:?}"
    );
    assert!(has_compiler_stage(&one));
}

#[test]
fn a_selection_plans_exactly_its_checks_stages_and_never_deadlocks() {
    assert_eq!(
        ids(&ctx(&[Check::Fmt], Scope::workspace())),
        [StageId::Formatting]
    );
    assert_eq!(
        ids(&ctx(&[Check::Test, Check::Doc], Scope::workspace())),
        [StageId::TestCompile, StageId::Test, StageId::Doctests]
    );
    assert_eq!(
        ids(&ctx(&[Check::Doc], Scope::workspace())),
        [StageId::Doctests]
    );
    let lone_doc = plan(&ctx(&[Check::Doc], Scope::workspace()));
    assert!(
        lone_doc[0].after_stages.is_empty(),
        "a stage never waits on one the run does not plan"
    );
    let mut selections: Vec<Vec<Check>> = every_check().into_iter().map(|c| vec![c]).collect();
    selections.extend([DEV.to_vec(), RELEASE.to_vec(), every_check()]);
    for checks in selections {
        for app in [false, true] {
            let c = if app {
                with_app(&checks, Scope::workspace())
            } else {
                ctx(&checks, Scope::workspace())
            };
            let p = plan(&c);
            crate::sched::check_waits(&p)
                .unwrap_or_else(|e| panic!("{checks:?} could deadlock: {e}"));
            let mut want = crate::checks::stages_of(&checks);
            // The measuring rows' compile is planned only where their matrices run and the
            // release's test compile does not build them ([`folds_measuring`]).
            want.retain(|id| *id != StageId::MeasurePrebuild || app);
            want.sort_by_key(|id| *id as u8);
            let mut got: Vec<StageId> = p.iter().map(|s| s.id).collect();
            got.sort_by_key(|id| *id as u8);
            assert_eq!(got, want, "{checks:?}, app {app}");
        }
    }
}

/// The pooled release must retain the same early-error boundary as an unpooled run.
/// NEGATIVE CONTROL: with every preflight complete, code generation is admitted.
#[cfg(unix)]
#[test]
fn pooled_codegen_waits_for_every_preflight() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 4).unwrap();
    let c = ctx(&RELEASE, Scope::workspace()).with_pool(Some(pool));
    let p = plan(&c);
    let compile = p.iter().position(|s| s.id == StageId::TestCompile).unwrap();
    let mut done = vec![false; p.len()];
    for id in [
        StageId::Formatting,
        StageId::Census,
        StageId::ExportScan,
        StageId::Tippy,
    ] {
        let Some(index) = p.iter().position(|s| s.id == id) else {
            continue;
        };
        assert!(
            !crate::sched::ready(&p, &done, &done, 0, compile, u64::MAX),
            "code generation can delay a preflight red at {id:?}"
        );
        done[index] = true;
    }
    assert!(crate::sched::ready(&p, &done, &done, 0, compile, u64::MAX));
}

#[test]
fn source_and_type_preflight_finish_before_test_codegen() {
    use crate::sched::{awaited, ready};
    let c = ctx(&RELEASE, Scope::workspace());
    let p = plan(&c);
    let at = |id| p.iter().position(|s| s.id == id).expect("planned");
    let (compile, test, doc) = (
        at(StageId::TestCompile),
        at(StageId::Test),
        at(StageId::Doctests),
    );
    assert_eq!(
        lane_dir(&c, Lane::AfterCompile),
        lane_dir(&c, Lane::MainTarget)
    );
    let preflight: Vec<_> = [
        StageId::Formatting,
        StageId::Census,
        StageId::ExportScan,
        StageId::Tippy,
    ]
    .into_iter()
    .filter_map(|id| p.iter().position(|s| s.id == id))
    .collect();
    let mut doc_waits = preflight.clone();
    doc_waits.push(compile);
    assert_eq!(awaited(&p, doc).collect::<Vec<_>>(), doc_waits);
    let none = vec![false; p.len()];
    assert!(!ready(&p, &none, &none, 0, doc, u64::MAX));
    assert!(!ready(&p, &none, &none, 0, compile, u64::MAX));
    assert!(
        !ready(&p, &none, &none, 0, test, u64::MAX),
        "the test run follows its compile"
    );
    // The guards and the lint wait for none of each other: each starts beside those running.
    let mut started = none.clone();
    for (running, &i) in preflight.iter().enumerate() {
        assert!(ready(&p, &started, &none, running, i, u64::MAX));
        started[i] = true;
    }
    let mut done = none.clone();
    for i in preflight {
        assert!(!ready(&p, &done, &done, 0, compile, u64::MAX));
        done[i] = true;
    }
    assert!(ready(&p, &done, &done, 0, compile, u64::MAX));
    done[compile] = true;
    let mut started = done.clone();
    assert!(ready(&p, &started, &done, 0, test, u64::MAX));
    assert!(ready(&p, &started, &done, 0, doc, u64::MAX));
    started[test] = true;
    assert!(ready(&p, &started, &done, 1, doc, u64::MAX));
}

/// THE GUARDS RUN TOGETHER. In the dev set the released shim, whose build's lane is its own, is
/// first and alone; formatting, the source guards, the export scan and the lint then wait for it
/// and for none of each other, so all four start beside one another, and everything after them
/// waits for all five. NEGATIVE CONTROL: with any one of the five unfinished the test compile
/// does not start, so no red of the preflight is found after test code generation began; and a
/// plan without the released shim starts the four at once.
#[test]
fn the_guards_and_the_lint_run_together_once_the_released_shim_is_done() {
    use crate::sched::{awaited, ready};
    let p = plan(&ctx(
        &DEV,
        Scope::changed("origin/main", vec!["seed".into()], true),
    ));
    let at = |id| p.iter().position(|s| s.id == id).expect("planned");
    let shim = at(StageId::ReleasedShim);
    let together = [
        StageId::Formatting,
        StageId::Census,
        StageId::ExportScan,
        StageId::Tippy,
    ]
    .map(at);
    let compile = at(StageId::TestCompile);
    assert!(p[shim].after_stages.is_empty());
    for i in together {
        assert_eq!(p[i].after_stages, [StageId::ReleasedShim], "{:?}", p[i].id);
        assert_eq!(awaited(&p, i).collect::<Vec<_>>(), [shim]);
    }
    let mut all = vec![shim];
    all.extend(together);
    assert_eq!(awaited(&p, compile).collect::<Vec<_>>(), all);

    let none = vec![false; p.len()];
    assert!(ready(&p, &none, &none, 0, shim, u64::MAX));
    for i in together {
        assert!(!ready(&p, &none, &none, 0, i, u64::MAX), "{:?}", p[i].id);
    }
    let mut done = none.clone();
    done[shim] = true;
    let mut started = done.clone();
    for (running, i) in together.into_iter().enumerate() {
        assert!(
            ready(&p, &started, &done, running, i, u64::MAX),
            "{:?} waits for a guard beside it",
            p[i].id
        );
        started[i] = true;
    }
    for unfinished in all.iter().copied() {
        let mut done = vec![false; p.len()];
        for &i in all.iter().filter(|&&i| i != unfinished) {
            done[i] = true;
        }
        assert!(
            !ready(&p, &done, &done, 0, compile, u64::MAX),
            "the test compile started before {:?} ended",
            p[unfinished].id
        );
        done[unfinished] = true;
        assert!(ready(&p, &done, &done, 0, compile, u64::MAX));
    }

    let no_shim = plan(&ctx(
        &[Check::Fmt, Check::Census, Check::ExportScan, Check::Lint],
        Scope::changed("origin/main", vec!["seed".into()], true),
    ));
    let none = vec![false; no_shim.len()];
    let mut started = none.clone();
    for i in 0..no_shim.len() {
        assert!(no_shim[i].after_stages.is_empty(), "{:?}", no_shim[i].id);
        assert!(ready(&no_shim, &started, &none, i, i, u64::MAX));
        started[i] = true;
    }
}

/// Export reuses the main target only after the test compile has populated it. An export-only
/// selection waits for nothing absent from its plan. NEGATIVE CONTROL: either the old trust
/// lane or an omitted dependency fails the paired plan.
#[test]
fn export_reuses_the_main_target_after_the_test_compile() {
    use crate::sched::awaited;

    let p = plan(&ctx(&[Check::Test, Check::Export], Scope::workspace()));
    let at = |id| p.iter().position(|s| s.id == id).expect("planned");
    let compile = at(StageId::TestCompile);
    let export = at(StageId::Export);
    assert_eq!(p[export].lane, Lane::MainTarget);
    assert_eq!(p[export].after_stages, [StageId::TestCompile]);
    assert_eq!(awaited(&p, export).collect::<Vec<_>>(), [compile]);

    let alone = plan(&ctx(&[Check::Export], Scope::workspace()));
    assert_eq!(alone.len(), 1);
    assert_eq!(alone[0].lane, Lane::MainTarget);
    assert!(alone[0].after_stages.is_empty());
}

fn with_app(checks: &[Check], scope: Scope) -> Ctx {
    ctx(checks, scope)
        .with_measure_artifact(Some(crate::MeasureArtifact::unnamed("/release/aterm.app")))
}

#[test]
fn the_measuring_compile_precedes_the_barrier_and_appkit_drives_the_main_target() {
    let p = plan(&with_app(&RELEASE, Scope::workspace()).with_release(true));
    let at = |id| p.iter().position(|s| s.id == id).expect("planned");
    let barrier = p.iter().position(|s| s.exclusive).expect("a barrier");
    assert_eq!(p[barrier].id, StageId::RedrawConformance);
    assert_eq!(p[barrier].lane, Lane::MainTarget);
    assert!(
        !p.iter().any(|s| s.id == StageId::MeasurePrebuild),
        "the release's test compile builds the measuring benches"
    );
    assert!(at(StageId::TestCompile) < barrier && !p[at(StageId::TestCompile)].exclusive);
    for id in [StageId::RedrawConformance, StageId::ObjcDrives] {
        assert!(barrier <= at(id), "{id:?} runs at or behind the barrier");
        assert!(at(id) < at(StageId::MeasuringTests));
        assert!(p[at(id)].exclusive);
        assert_eq!(p[at(id)].lane, Lane::MainTarget, "{id:?}");
    }
    let appkit = plan(&ctx(&[Check::Appkit], Scope::workspace()));
    for stage in &appkit {
        assert!(
            stage.id.timed() && stage.id.opens_windows(),
            "{:?}",
            stage.id
        );
        assert!(
            !stage.exclusive,
            "an ordinary AppKit run needs the quiet/window reservations, not a global barrier"
        );
    }
    let alone = plan(&with_app(&[Check::Measure], Scope::workspace()));
    let alone_ids: Vec<StageId> = alone.iter().map(|s| s.id).collect();
    assert_eq!(
        alone_ids,
        [
            StageId::MeasurePrebuild,
            StageId::MeasuringTests,
            StageId::GuiSmoke
        ]
    );
    let barrier = alone.iter().position(|s| s.exclusive).expect("a barrier");
    assert_eq!(alone[barrier].id, StageId::MeasuringTests);
    assert!(!alone[0].exclusive && alone[0].lane == Lane::MainTarget);
    assert_eq!(
        ids(&ctx(&[Check::Measure], Scope::workspace())),
        [StageId::MeasuringTests, StageId::GuiSmoke],
        "with no bundle no matrix runs, so nothing compiles them"
    );
}

#[test]
fn only_the_releases_test_compile_builds_the_measuring_benches() {
    let prebuilds = |c: &Ctx| ids(c).contains(&StageId::MeasurePrebuild);
    let folding = [
        // A prebuild would be planned here without the fold.
        with_app(&RELEASE, Scope::workspace()).with_release(true),
        // `verify release` with no bundle builds what the release's does.
        ctx(&RELEASE, Scope::workspace()).with_release(true),
        // The compile `ship release` runs first, so its `verify` finds them built, and the
        // command a release's red test row prints.
        ctx(&[Check::Test], Scope::workspace())
            .with_checkers_in_tests(true)
            .with_compile_only(true),
        ctx(&[Check::Test], Scope::workspace()).with_checkers_in_tests(true),
    ];
    for c in &folding {
        assert!(folds_measuring(c), "{:?}", c.checks);
        assert!(!prebuilds(c), "{:?}", c.checks);
    }
    let narrowed = Scope::packages(&["aterm-verify".to_string()]);
    let elsewhere = [
        // A dev run that tests and measures: its red compile reruns as `verify test`.
        (
            with_app(&[Check::Test, Check::Measure], Scope::workspace()),
            true,
        ),
        (with_app(&[Check::Test, Check::Measure], narrowed), true),
        (with_app(&[Check::Measure], Scope::workspace()), true),
        // No bundle, so no matrix runs: nothing compiles them.
        (
            ctx(&[Check::Test, Check::Measure], Scope::workspace()),
            false,
        ),
        (ctx(&[Check::Test], Scope::workspace()), false),
        (
            with_app(&[Check::Measure], Scope::workspace()).with_cursor_pacing(true),
            false,
        ),
    ];
    for (c, prebuild) in &elsewhere {
        assert!(!folds_measuring(c), "{:?}", c.checks);
        assert_eq!(prebuilds(c), *prebuild, "{:?}", c.checks);
    }
}

#[test]
fn appkit_failure_is_observed_before_measurements_and_all_checks_still_run() {
    let stages = plan(&ctx(&[Check::Appkit, Check::Measure], Scope::workspace()));
    let seen = std::sync::Mutex::new(Vec::new());
    let reports = crate::sched::run_stages(
        &stages,
        u64::MAX,
        &crate::sched::Halt::off(),
        |stage, _| {
            seen.lock().unwrap().push(stage.id);
            let mut report = crate::ladder::Report::new(stage.title.clone());
            if stage.id == StageId::ObjcDrives {
                report.fail("fixture: toolbar state missing");
            } else {
                report.pass("fixture");
            }
            report
        },
        |_, _| {},
    );
    let seen = seen.into_inner().unwrap();
    let position = |id| seen.iter().position(|seen| *seen == id).unwrap();
    assert!(position(StageId::ObjcDrives) < position(StageId::MeasuringTests));
    assert_eq!(reports.len(), stages.len());
    assert_eq!(seen.len(), stages.len());
}

#[test]
fn checkers_reuse_the_test_compile_and_wait_for_the_plain_run() {
    for checks in [vec![Check::Checkers], vec![Check::Test, Check::Checkers]] {
        let p = plan(&ctx(&checks, Scope::workspace()));
        assert_eq!(
            p.iter().filter(|s| crate::memory::compiles(s.id)).count(),
            1
        );
        let checker = p.iter().find(|s| s.id == StageId::SpecCheckers).unwrap();
        assert_eq!(checker.lane, Lane::MainTarget);
        assert!(checker.after_stages.contains(&StageId::TestCompile));
        assert_eq!(
            checker.after_stages.contains(&StageId::Test),
            checks.contains(&Check::Test)
        );
    }
    assert!(
        plan(&ctx(&RELEASE, Scope::workspace()))
            .iter()
            .all(|s| s.id != StageId::SpecCheckers)
    );
}
