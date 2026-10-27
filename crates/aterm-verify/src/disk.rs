// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK A RUN WILL WRITE, CLAIMED BEFORE IT BUILDS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! DISK-ADMIT).
//!
//! The preflight that refused a run short of disk went with the snapshot gate (42e8a9a273), and
//! since then a short volume showed only at `ENOSPC`, hours in, as a COULD NOT RUN row
//! ([`crate::exec::Run::environment_failure`]). Now a run whose checks build claims, in the
//! capacity ledger, what its directories (the main lane's build directory and `<target>/verify`)
//! grew by in the last runs of this target ([`FOOTPRINT`]), or where none was measured
//! [`COLD_BYTES`] less what they hold already. A volume short of it is relieved first (the build
//! cache's pass under pressure, and inside aterm the window's disk watch), then waited for with a
//! line a minute naming the need, the free space and the largest holders (`atpkg::admit_disk`).
//! `--admit now` starts at once and says what it lacks. Only a need above what the volume holds
//! over its floor is refused.

use std::path::{Path, PathBuf};

use crate::{Check, Ctx};

/// One gibibyte.
const GIB: u64 = 1 << 30;

/// The fallback for a run without measured footprints: 72 GiB. On 2026-10-27, the whole
/// workspace at `932f1ef624` grew empty tracked directories by 61.1 GiB; AppKit then brought
/// their peak to 63.2 GiB (`docs/measured/verify-disk-2026-10-27.md`). Round up with room for
/// growth. A first run claims this less what its directories hold already; a run measured
/// here claims what it wrote instead.
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

/// The run's disk as it is claimed: its build directory and `<target>/verify`, measured, on the
/// target's volume. `None` when no check it runs builds.
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
    let scope = if ctx.checks.iter().any(|c| c.narrows()) {
        format!(" {}", ctx.scope.label())
    } else {
        String::new()
    };
    Some(atpkg::admit_disk::Run {
        what: format!("verify {}{scope}", names.join(" ")),
        dirs,
        record: ctx.state_dir().join(FOOTPRINT),
        cold: COLD_BYTES,
        on: ctx.target_dir.clone(),
    })
}

/// Claim the run's disk ([`run_of`]) before it builds, saying on stderr what it does and waits
/// for. `Ok(None)`: nothing to claim, or no ledger to claim in (said, and the run goes on).
///
/// # Errors
/// The need is more than the target's volume holds above its floor: the run can never fit there;
/// or an interrupt ended the wait.
pub fn admit(
    ctx: &Ctx,
    mode: atpkg::admit_disk::Admit,
) -> Result<Option<atpkg::admit_disk::Admitted>, String> {
    let Some(run) = run_of(ctx) else {
        return Ok(None);
    };
    match atpkg::admit_disk::admit(run, mode, &mut |line| eprintln!("verify: {line}")) {
        Ok(held) => Ok(Some(held)),
        Err(atpkg::admit_disk::Error::Never(why)) => Err(format!(
            "{why}; it can never be admitted there. Fix: point CARGO_TARGET_DIR at a larger volume"
        )),
        Err(e @ atpkg::admit_disk::Error::Stopped { .. }) => Err(format!(
            "the run's disk claim was {e}; no check started, so nothing was decided"
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
}
