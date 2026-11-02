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

/// The run's disk as it is claimed: its build directory and `<target>/verify`, measured, on the
/// target's volume, kept by its scope ([`scope_of`]). `None` when no check it runs builds.
#[must_use]
pub fn run_of(ctx: &Ctx) -> Option<atpkg::admit_disk::Run> {
    if !ctx.checks.iter().any(|c| builds(*c)) {
        return None;
    }
    let mut dirs: Vec<PathBuf> = ctx
        .main_build_dir
        .as_deref()
        .and_then(|v| build_dir_path(v, &ctx.root))
        .into_iter()
        .collect();
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
}
