// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RELEASE'S ONE DISK CLAIM (`docs/DESIGN-fast-build-release-2026-11-05.md`, A4).
//!
//! A release used to claim no disk: it compared `df` with 30 GiB (its clone's target warm) or 39
//! GiB (cold) once, and then each `verify` it ran claimed for itself, sizing its claim by walking
//! the clone's shared-cache build directory as it was admitted, once a minute while it ran and
//! once as it ended: hundreds of GiB read file by file, several times a release. Now the release
//! claims once, in the capacity ledger, for all it writes, sized from its own record of what its
//! runs wrote ([`need`], [`RECORD`]) rather than by any walk, and hands its children the claim's
//! name (`aterm_capacity::DISK_COVER_ENV`), which covers each `verify` it runs (they walk nothing
//! and claim nothing: `aterm_verify::disk::Disk::Covered`). The claim waits for room as any claim
//! does, relieved first by the build cache's pass, which keeps the release's own directories
//! ([`own_dirs`]), and gives up behind holders that made no progress for
//! [`aterm_toolchain::stall::STALLED_AFTER`].

use std::path::{Path, PathBuf};

use atpkg::admit_disk::{Admit, Error, SizedClaim, Whole};

use crate::release::Refusal;

/// One gibibyte.
const GIB: u64 = 1 << 30;

/// The least a release claims where its clone's build is warm: the build and `verify release`
/// still to come, measured before the claim existed (the `df` comparison it replaced).
pub const WARM: u64 = 30 * GIB;

/// The least a release claims where its clone's build is cold.
pub const COLD: u64 = 39 * GIB;

/// What a stage claims outside the release's claim, for itself alone (a stage again before the
/// publish, or a stage of a candidate verified earlier): the free space a release that verified
/// was always held to ([`crate::gates::MIN_FREE_DISK_GIB`]), as no build or `verify release`
/// follows it. Measured on m3 (2026-11-06), a stage's compiles wrote 1077 MiB (0.157.0) and
/// 365 MiB (v0.156.0), and 0.157.0's stage again before its publish wrote none.
pub const STAGE: u64 = crate::gates::MIN_FREE_DISK_GIB * GIB;

/// The file under the release's state directory (`~/.local/state/aterm-release`) keeping what
/// this machine's last releases wrote (`aterm_capacity::Footprints`, [`aterm_capacity::footprint::KEPT`]
/// runs): a record of what a release writes, not a cache.
pub const RECORD: &str = "disk-footprint.json";

/// The scope the record keeps a release's runs under (`atpkg::admit_disk::scope_tag`).
pub const SCOPE: &str = "release";

/// The record in the release's state directory `state`.
#[must_use]
pub fn record_in(state: &Path) -> PathBuf {
    state.join(RECORD)
}

/// What the next release claims: the most any of the last runs kept in `kept` wrote, or the floor
/// ([`WARM`] where its clone's build is `warm`, else [`COLD`]) where that is more, so a record of
/// small runs never claims less than a release was always held to.
#[must_use]
pub fn need(kept: &aterm_capacity::Footprints, warm: bool) -> aterm_capacity::Estimate {
    let floor = if warm { WARM } else { COLD };
    let e = kept.need(0, floor, 0, atpkg::admit_disk::scope_tag(SCOPE));
    aterm_capacity::Estimate {
        bytes: e.bytes.max(floor),
        ..e
    }
}

/// What sized a claim of `bytes` ([`need`]) for a clone whose build is `warm`, as the release
/// says it.
#[must_use]
pub fn basis(bytes: u64, warm: bool) -> &'static str {
    let floor = if warm { WARM } else { COLD };
    if bytes > floor {
        "the most a recorded release on this Mac wrote"
    } else if warm {
        "the floor where the clone's build is warm"
    } else {
        "the floor where the clone's build is cold"
    }
}

/// The runs `path` keeps; none where it is absent or unreadable.
#[must_use]
pub fn load(path: &Path) -> aterm_capacity::Footprints {
    aterm_capacity::Footprints::load(path)
}

/// Keep in `path` that a release that passed wrote `bytes`, the oldest past
/// [`aterm_capacity::footprint::KEPT`] let go.
///
/// # Errors
/// The record could not be written (a full disk): the runs kept before stand.
pub fn keep(path: &Path, bytes: u64) -> std::io::Result<()> {
    let mut kept = load(path);
    kept.push(aterm_capacity::Footprint {
        peak: bytes,
        grew: bytes,
        at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        dirs: None,
        scope: atpkg::admit_disk::scope_tag(SCOPE),
    });
    kept.save(path)
}

/// The directories the release writes into, which the relief of its claim keeps: the build
/// directory its `verify` runs build in (the clone's in the shared build cache, read as those runs
/// read it: `crate::release_steps::verify_plan`) and the release cache's target root, one
/// `CARGO_TARGET_DIR` per slice under it (`crate::snapshot::CacheRoot::target`). One that cannot
/// be named is left out.
#[must_use]
pub fn own_dirs(repo: &Path) -> Vec<PathBuf> {
    let root = aterm_verify::physical(repo);
    crate::release_steps::verify_plan(repo)
        .ok()
        .and_then(|plan| {
            plan.build_dir()
                .and_then(|v| aterm_verify::disk::build_dir_path(v, &root))
        })
        .into_iter()
        .chain(
            crate::snapshot::CacheRoot::for_repo(repo)
                .ok()
                .map(|c| c.target()),
        )
        .collect()
}

/// The claim of `bytes` for the release `what` cutting from the clone `repo`: on the clone's
/// volume, of the clone's checkout, keeping `keep` through relief, and given up behind holders
/// that made no progress for [`aterm_toolchain::stall::STALLED_AFTER`].
#[must_use]
pub fn sized(repo: &Path, bytes: u64, what: &str, keep: Vec<PathBuf>) -> SizedClaim {
    SizedClaim {
        what: what.to_owned(),
        bytes,
        on: repo.to_path_buf(),
        keep,
        checkout: Some(repo.display().to_string()),
        stuck_ends: Some(aterm_toolchain::stall::STALLED_AFTER),
    }
}

/// Claim `bytes` for the release `what` cutting from `repo`, in this user's ledger on the live
/// machine, waiting for room and saying through `say` what it does and waits for. `None` where no
/// ledger could take the claim (said): the release goes on unclaimed, and each `verify` it runs
/// claims for itself.
///
/// # Errors
/// As [`answer`].
pub fn claim(
    repo: &Path,
    bytes: u64,
    what: &str,
    say: &mut dyn FnMut(&str),
) -> Result<Option<Whole>, Refusal> {
    let run = sized(repo, bytes, what, own_dirs(repo));
    answer(atpkg::admit_disk::admit_sized(run, Admit::Wait, say), say)
}

/// What the release does with its claim's answer: holds the claim; or, where the ledger could not
/// take it, says so and goes on unclaimed (`None`).
///
/// # Errors
/// The refusal that stops the release: the claim can never fit on the clone's volume; or, with
/// nothing about the code decided (resumable), it is more than the volume has free above its
/// floor once its relief ran, which no wait admits, an interrupt ended its wait, or it gave up
/// behind holders that made no progress.
pub fn answer(
    admitted: Result<Whole, Error>,
    say: &mut dyn FnMut(&str),
) -> Result<Option<Whole>, Refusal> {
    match admitted {
        Ok(whole) => Ok(Some(whole)),
        Err(e @ Error::Never(_)) => Err(Refusal::new(
            format!("the release's disk claim: {e}"),
            "free space on the clone's volume, or cut from a clone on a larger one, then re-run",
        )),
        Err(e @ Error::Unmet { .. }) => Err(Refusal::new(
            format!("the release's disk claim was refused, nothing decided: {e}"),
            "free what the claim names on the clone's volume, or cut from a clone on a larger \
             one, then re-run",
        )
        .could_not_run()),
        Err(e @ (Error::Stopped { .. } | Error::Stuck { .. } | Error::Refused(_))) => {
            Err(Refusal::new(
                format!("the release's disk claim {e}; nothing was decided"),
                "re-run once the volume has room",
            )
            .could_not_run())
        }
        Err(e @ (Error::Ledger(_) | Error::Timing(_))) => {
            say(&format!(
                "disk: the release's disk is not claimed ({e}); it runs without a claim, and \
                 each verify it runs claims for itself"
            ));
            Ok(None)
        }
    }
}

#[cfg(test)]
#[path = "release_disk_tests.rs"]
mod tests;
