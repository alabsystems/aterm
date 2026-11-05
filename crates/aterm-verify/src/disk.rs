// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK A RUN WILL WRITE, CLAIMED BEFORE IT BUILDS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! DISK-ADMIT).
//!
//! The preflight that refused a run short of disk went with the snapshot gate (42e8a9a273), and
//! since then a short volume showed only at `ENOSPC`, hours in, as a COULD NOT RUN row
//! ([`crate::exec::Run::environment_failure`]). Now a run whose checks build claims, in the
//! capacity ledger, what its directories (the main lane's build directory and `<target>/verify`)
//! grew by in the last runs of its scope on this target ([`scope_of`]: the checks that build and
//! the packages they read), or held at their peak beyond what they hold now
//! (a build directory reclaimed since is written again) but no more than [`COLD_BYTES`]
//! ([`FOOTPRINT`]), or where none was measured [`COLD_BYTES`] less what they hold already. A peak
//! further above what they hold measured more than one run writes (stale builds, or the build
//! root before the cache moved), so it claims the budget. A volume short of it is relieved first
//! (the build cache's pass under pressure, and inside aterm the window's disk watch), then waited
//! for with a line a minute naming the need, the free space and the largest holders, after one
//! saying why it needs that (`atpkg::admit_disk`).
//! `--admit now` starts at once and says what it lacks. Only a need above what the volume holds
//! over its floor is refused. A need above what it has free over its floor now waits for space
//! freed, which no other run's end gives, and reserves nothing against the runs behind it.
//!
//! Those directories are each checkout's own (the shared cache keeps one build directory per
//! workspace path), so a fresh worktree's hold no run of anything and its record beside them none:
//! every first run claimed [`COLD_BYTES`] less the verify tool's own build, 72 GiB for a scoped run
//! that wrote 3 GiB, and waited on a Mac with 170 GB free (V1, DA-MACHINE-FOOTPRINT). So a run that
//! passed is kept in the machine's record too, under this user's aterm state directory
//! ([`MACHINE_FOOTPRINT`]), by what it built ([`selection_of`]), and a run with no run of its scope
//! on its own directories claims what runs in this machine's checkouts bound
//! (`aterm_capacity::MachineFootprints::cover`): its own selection's run, a wider one's, or those of
//! selections whose crates together hold its, where that is less than the cold budget less what its
//! directories hold.

use std::path::{Path, PathBuf};

use crate::{Check, Ctx};

/// One gibibyte.
const GIB: u64 = 1 << 30;

/// The fallback for a run without measured footprints: 72 GiB. On 2026-10-27, the whole
/// workspace at `932f1ef624` grew empty tracked directories by 61.1 GiB; AppKit then brought
/// their peak to 63.2 GiB (`docs/measured/verify-disk-2026-10-27.md`). Round up with room for
/// growth. A first run claims this less what its directories hold already; a run measured
/// here claims what it wrote instead, and at most this to write again what they held at its
/// peak.
pub const COLD_BYTES: u64 = 72 * GIB;

/// The file in `<target>/verify` keeping what the last runs wrote (`aterm_capacity::Footprints`).
pub const FOOTPRINT: &str = "footprint.json";

/// The file under this user's aterm state directory (`aterm_types::dirs::account_state_dir`)
/// keeping what the runs that passed in every checkout on this machine wrote
/// (`aterm_capacity::MachineFootprints`), beside the stages' memory
/// (`aterm_capacity::book::VERIFY_RECORD`): a record of what a scope writes, not a cache, so a
/// build cache emptied leaves it true; and a test binary's state directory is its own.
pub const MACHINE_FOOTPRINT: &str = "verify/disk-footprint.json";

/// Whether `check` builds into the run's directories: every check but those that only read the
/// tree (fmt, census, forge) or the network (export).
#[must_use]
pub const fn builds(check: Check) -> bool {
    !matches!(
        check,
        Check::Fmt | Check::Census | Check::Forge | Check::Export
    )
}

/// The directory a `build.build-dir='…'` value names for the checkout at `root`.
#[must_use]
pub fn build_dir_path(value: &str, root: &Path) -> Option<PathBuf> {
    let dir = value
        .strip_prefix("build.build-dir='")?
        .strip_suffix('\'')?;
    Some(PathBuf::from(
        dir.replace("{workspace-root}", &root.to_string_lossy()),
    ))
}

/// What a run builds, spelled the same each time it builds the same, which its footprints are
/// kept by (`atpkg::admit_disk::Run::scope`): the checks it runs that build, sorted; where one
/// of them reads the selection, the packages it names, sorted, the whole workspace, or no
/// package (an empty cone); and the cells `--cell` chose. A `-p` run and a wider run of the same
/// checks are two scopes, so neither's growth is what the other claims.
#[must_use]
pub fn scope_of(ctx: &Ctx) -> String {
    let building = || ctx.checks.iter().copied().filter(|c| builds(*c));
    let mut checks: Vec<&str> = building().map(Check::name).collect();
    checks.sort_unstable();
    checks.dedup();
    let mut scope = checks.join(" ");
    if building().any(|c| c.reads(&ctx.scope)) {
        if ctx.scope.is_workspace() {
            scope.push_str(" --workspace");
        } else if ctx.scope.selects_nothing() {
            scope.push_str(" <no crates selected>");
        } else {
            let mut crates = ctx.scope.crates().to_vec();
            crates.sort_unstable();
            crates.dedup();
            for c in crates {
                scope.push_str(" -p ");
                scope.push_str(&c);
            }
        }
    }
    if !ctx.cells.is_empty() && building().any(|c| c == Check::Cells) {
        let mut cells = ctx.cells.clone();
        cells.sort_unstable();
        cells.dedup();
        scope.push_str(" --cell ");
        scope.push_str(&cells.join(","));
    }
    scope
}

/// What a run builds as the machine's record keeps it (`atpkg::admit_disk::Machinewide`), so a
/// run in another checkout bounds it only where it did the same work: its work is the checks it
/// runs that build, sorted, each that does not read the selection marked `:tree` (what it builds
/// does not follow `-p`'s crates: `cells` named under the change cone lints every cell's whole
/// graph, and cells the cone pulled lint the cone closed over its dependents in each cell's
/// graph, more than a `-p` run of the same crates does), then whatever else changes what they
/// write, as a rerun's command says it: the cells `--cell` chose, cells the
/// selection pulled, `--compile-only`, `--no-windows` where a check opens windows, and
/// `--cursor-pacing` and `--app` for `measure`. Its crates are the selection's, sorted and each
/// once (none for the empty cone); `None` for the whole workspace, or where no check that builds
/// reads the selection.
#[must_use]
pub fn selection_of(ctx: &Ctx) -> atpkg::admit_disk::Selection {
    let building: Vec<Check> = ctx.checks.iter().copied().filter(|c| builds(*c)).collect();
    let has = |check: Check| building.contains(&check);
    let mut checks: Vec<String> = building
        .iter()
        .map(|c| {
            if c.reads(&ctx.scope) {
                c.name().to_owned()
            } else {
                format!("{}:tree", c.name())
            }
        })
        .collect();
    checks.sort_unstable();
    checks.dedup();
    let mut work = checks.join(" ");
    if has(Check::Cells) && !ctx.cells.is_empty() {
        let mut cells = ctx.cells.clone();
        cells.sort_unstable();
        cells.dedup();
        work.push_str(" --cell ");
        work.push_str(&cells.join(","));
    }
    if has(Check::Cells) && ctx.cells_pulled {
        work.push_str(" pulled-cells");
    }
    if ctx.compile_only && (has(Check::Test) || has(Check::Checkers)) {
        work.push_str(" --compile-only");
    }
    if ctx.no_windows
        && building
            .iter()
            .any(|c| c.spec().stages.iter().any(|id| id.opens_windows()))
    {
        work.push_str(" --no-windows");
    }
    if has(Check::Measure) && ctx.cursor_pacing {
        work.push_str(" --cursor-pacing");
    }
    if has(Check::Measure) && ctx.measure_artifact.is_some() {
        work.push_str(" --app");
    }
    let crates =
        (building.iter().any(|c| c.reads(&ctx.scope)) && !ctx.scope.is_workspace()).then(|| {
            let mut crates = ctx.scope.crates().to_vec();
            crates.sort_unstable();
            crates.dedup();
            crates
        });
    atpkg::admit_disk::Selection { work, crates }
}

/// The run's disk as it is claimed: its build directory and `<target>/verify`, measured, on the
/// target's volume, kept by its scope ([`scope_of`]) there, and by what it builds
/// ([`selection_of`]) in the machine's record ([`MACHINE_FOOTPRINT`]) where this user has a state
/// directory and the build directory is the one verify places ([`Ctx::main_build_dir`]): one
/// the caller chose is not measured, so a run there grows `<target>/verify` alone, which bounds
/// nothing another checkout's build writes. `None` when no check it runs builds.
#[must_use]
pub fn run_of(ctx: &Ctx) -> Option<atpkg::admit_disk::Run> {
    if !ctx.checks.iter().any(|c| builds(*c)) {
        return None;
    }
    let build = ctx
        .main_build_dir
        .as_deref()
        .and_then(|v| build_dir_path(v, &ctx.root));
    let measured = build.is_some();
    let mut dirs: Vec<PathBuf> = build.into_iter().collect();
    dirs.push(ctx.state_dir());
    let names: Vec<&str> = ctx.checks.iter().map(|c| c.name()).collect();
    let scope = if ctx.checks.iter().any(|c| c.reads(&ctx.scope)) {
        format!(" {}", ctx.scope.label())
    } else {
        String::new()
    };
    Some(atpkg::admit_disk::Run {
        what: format!("verify {}{scope}", names.join(" ")),
        scope: scope_of(ctx),
        dirs,
        record: ctx.state_dir().join(FOOTPRINT),
        cold: COLD_BYTES,
        on: ctx.target_dir.clone(),
        stuck_ends: Some(crate::STUCK_ENDS),
        machine: aterm_types::dirs::account_state_dir()
            .filter(|_| measured)
            .map(|state| atpkg::admit_disk::Machinewide {
                record: state.join(MACHINE_FOOTPRINT),
                selection: selection_of(ctx),
            }),
    })
}

/// Claim the run's disk ([`run_of`]) before it builds, saying on stderr what it does and waits
/// for. `Ok(None)`: nothing to claim, or no ledger to claim in (said, and the run goes on).
///
/// # Errors
/// As [`answer`].
pub fn admit(
    ctx: &Ctx,
    mode: atpkg::admit_disk::Admit,
) -> Result<Option<atpkg::admit_disk::Admitted>, String> {
    let Some(run) = run_of(ctx) else {
        return Ok(None);
    };
    answer(atpkg::admit_disk::admit(run, mode, &mut |line| {
        eprintln!("verify: {line}");
    }))
}

/// What the run does with its disk claim's answer: holds the claim while it builds; or, where no
/// ledger could take it, says so and runs without one.
///
/// # Errors
/// Why the run COULD NOT RUN, no check started: the need is more than the target's volume holds
/// above its floor, so the run can never fit there; an interrupt ended the wait; or the wait gave
/// up behind holders that made no progress for [`crate::STUCK_ENDS`], named, and left alone.
fn answer(
    admitted: Result<atpkg::admit_disk::Admitted, atpkg::admit_disk::Error>,
) -> Result<Option<atpkg::admit_disk::Admitted>, String> {
    match admitted {
        Ok(held) => Ok(Some(held)),
        Err(atpkg::admit_disk::Error::Never(why)) => Err(format!(
            "{why}; it can never be admitted there. Fix: point CARGO_TARGET_DIR at a larger volume"
        )),
        Err(e @ atpkg::admit_disk::Error::Stopped { .. }) => Err(format!(
            "the run's disk claim was {e}; no check started, so nothing was decided"
        )),
        Err(e @ atpkg::admit_disk::Error::Stuck { .. }) => Err(format!(
            "the run's disk claim {e}; no check started, so nothing was decided"
        )),
        Err(e) => {
            eprintln!("verify: the run's disk is not claimed ({e}); it runs without a claim");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let run = run_of(&lint).expect("lint builds");
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
        let reads = ctx().with_checks(&[Check::Fmt, Check::Census, Check::Forge]);
        assert_eq!(run_of(&reads), None);
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
            run_of(&ctx).and_then(|r| r.stuck_ends),
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
        assert!(matches!(answer(Err(unusable)), Ok(None)));
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
        assert_eq!(narrow, "lint test -p a -p b");
        assert_eq!(
            at(p(&["a", "b"]), &[Check::Fmt, Check::Lint, Check::Test]),
            narrow
        );
        assert_eq!(
            at(crate::Scope::workspace(), &[Check::Lint, Check::Test]),
            "lint test --workspace"
        );
        assert_ne!(at(p(&["a"]), &[Check::Lint, Check::Test]), narrow);
        assert_ne!(at(p(&["a", "b"]), &[Check::Lint]), narrow);
        // `-p` narrows `cells`, so its growth is no claim on the whole graph's run.
        assert_eq!(at(p(&["a"]), &[Check::Cells]), "cells -p a");
        assert_eq!(at(crate::Scope::workspace(), &[Check::Cells]), "cells");
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
        .expect("lint builds");
        assert_eq!(run.scope, "lint -p a");
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
            work: work.to_owned(),
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
            sel("test --compile-only", Some(&["a"]))
        );
        assert_eq!(
            selection_of(&at(p(&["a"]), &[Check::Lint]).with_no_windows(true)),
            sel("lint", Some(&["a"])),
            "no check of it opens windows"
        );
        assert_eq!(
            selection_of(&at(p(&["a"]), &[Check::Appkit]).with_no_windows(true)),
            sel("appkit:tree --no-windows", None)
        );
        assert_eq!(
            selection_of(
                &at(crate::Scope::workspace(), &[Check::Cells])
                    .with_cells(vec!["win".to_owned(), "linux".to_owned()])
            ),
            sel("cells:tree --cell linux,win", None)
        );
    }

    /// A run that builds names the machine's record under this user's aterm state directory (a
    /// test binary's own here), by its selection, where it measures the build directory verify
    /// places. NEGATIVE CONTROL: a build directory the caller chose is not measured, so its runs,
    /// which grew `<target>/verify` alone, are kept for no other checkout.
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
        let run = run_of(&ctx).expect("a run that builds");
        assert_eq!(run.dirs[0], PathBuf::from("/c/w/aterm/.cargo-build"));
        let machine = run.machine.expect("kept machine-wide");
        let state = aterm_types::dirs::account_state_dir().expect("a test binary's state");
        assert_eq!(machine.record, state.join(MACHINE_FOOTPRINT));
        assert_eq!(machine.selection, selection_of(&ctx));
        ctx.main_build_dir = None;
        let run = run_of(&ctx).expect("a run that builds");
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
            crate::let_disk_go(Some(held), passed, Some(&mut log));
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
}
