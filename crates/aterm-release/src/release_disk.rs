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
/// read it: `crate::release_steps::verify_layout`) and the release cache's target root, one
/// `CARGO_TARGET_DIR` per slice under it (`crate::snapshot::CacheRoot::target`). One that cannot
/// be named is left out.
#[must_use]
pub fn own_dirs(repo: &Path) -> Vec<PathBuf> {
    let root = aterm_verify::physical(repo);
    crate::release_steps::verify_layout(repo)
        .1
        .and_then(|v| aterm_verify::disk::build_dir_path(&v, &root))
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
/// nothing about the code decided (resumable), an interrupt ended its wait, or it gave up behind
/// holders that made no progress.
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
        Err(e @ (Error::Stopped { .. } | Error::Stuck { .. })) => Err(Refusal::new(
            format!("the release's disk claim {e}; nothing was decided"),
            "re-run once the volume has room",
        )
        .could_not_run()),
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
mod tests {
    use super::*;

    fn run(gib: u64) -> aterm_capacity::Footprint {
        aterm_capacity::Footprint {
            peak: gib * GIB,
            grew: gib * GIB,
            at_ms: 1,
            dirs: None,
            scope: atpkg::admit_disk::scope_tag(SCOPE),
        }
    }

    /// A RELEASE CLAIMS THE MOST ITS LAST RUNS WROTE, AND NEVER LESS THAN ITS FLOOR: none kept, the
    /// cold or warm floor; runs that wrote less, the floor; a run that wrote more, what it wrote,
    /// until eight later runs let it go. NEGATIVE CONTROL: seven later runs keep it.
    #[test]
    fn a_release_claims_the_most_its_last_runs_wrote_above_its_floor() {
        let none = aterm_capacity::Footprints::default();
        assert_eq!(need(&none, false).bytes, 39 * GIB);
        assert_eq!(need(&none, true).bytes, 30 * GIB);

        let mut kept = aterm_capacity::Footprints::default();
        kept.push(run(20));
        assert_eq!(need(&kept, true).bytes, 30 * GIB);
        assert_eq!(need(&kept, false).bytes, 39 * GIB);
        kept.push(run(45));
        assert_eq!(need(&kept, true).bytes, 45 * GIB);
        assert_eq!(need(&kept, true).runs, 2);

        for _ in 0..7 {
            kept.push(run(20));
        }
        assert_eq!(
            need(&kept, true).bytes,
            45 * GIB,
            "seven later runs keep it"
        );
        kept.push(run(20));
        assert_eq!(need(&kept, true).bytes, 30 * GIB, "the eighth lets it go");

        let mut other = aterm_capacity::Footprints::default();
        other.push(aterm_capacity::Footprint {
            scope: atpkg::admit_disk::scope_tag("lint --workspace"),
            ..run(80)
        });
        assert_eq!(need(&other, true).bytes, 30 * GIB, "another scope's run");
    }

    /// WHAT A RELEASE WROTE IS KEPT IN THE MACHINE'S RECORD and read back: none where the file is
    /// absent, each run kept in order, and the next release's claim reads them.
    #[test]
    fn what_a_release_wrote_is_kept_and_read_back() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("release-disk-record-")
            .tempdir()
            .expect("scratch");
        let path = record_in(&dir.path().join("state"));
        assert!(load(&path).runs().is_empty());
        keep(&path, 41 * GIB).expect("kept");
        keep(&path, 33 * GIB).expect("kept");
        let kept = load(&path);
        assert_eq!(
            kept.runs().iter().map(|r| r.grew).collect::<Vec<_>>(),
            vec![41 * GIB, 33 * GIB]
        );
        assert_eq!(need(&kept, true).bytes, 41 * GIB);
    }

    fn room(dir: &Path, free_gib: u64) -> aterm_capacity::Fixed {
        let mut reading = aterm_capacity::Reading::of_gib(48, 40, 14);
        reading.volume = Some(aterm_capacity::Volume {
            mount: dir.to_path_buf(),
            total: 400 * GIB,
            free: free_gib * GIB,
            floor: 10 * GIB,
            room: 2 * GIB,
        });
        aterm_capacity::Fixed::new(reading)
    }

    fn claim_on(
        sized: SizedClaim,
        ledger: &aterm_capacity::Ledger,
        machine: &aterm_capacity::Fixed,
        said: &mut Vec<String>,
    ) -> Result<Option<Whole>, Refusal> {
        let mut say = |l: &str| said.push(l.to_owned());
        let admitted = atpkg::admit_disk::admit_sized_on(
            sized,
            ledger,
            machine,
            Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut atpkg::admit_disk::Reclaim::of(
                Box::new(|_, _| (0, Vec::new(), Vec::new())),
                Box::new(|_, _, _| None),
                Box::new(|_, _| Vec::new()),
            ),
            &mut say,
        );
        answer(admitted, &mut say)
    }

    /// THE RELEASE'S CLAIM IS ONE HELD DISK CLAIM ITS VERIFY RUNS ARE COVERED BY: of the clone's
    /// volume and checkout, keeping its directories, given up past the stall bound; held, its name
    /// covers a child of the release, and it is let go as it drops. One that can never fit refuses
    /// the release, a decided refusal; one the ledger cannot take goes on unclaimed, said.
    #[test]
    fn the_release_claim_is_one_held_disk_claim_its_runs_are_covered_by() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("release-disk-claim-")
            .tempdir()
            .expect("scratch");
        let repo = dir.path().join("aterm-release-clone");
        let keep = vec![dir.path().join("cache"), dir.path().join("target")];
        let s = sized(&repo, 39 * GIB, "release 0.156.0", keep.clone());
        assert_eq!(
            (&s.on, &s.keep, s.checkout.as_deref(), s.stuck_ends),
            (
                &repo,
                &keep,
                Some(repo.display().to_string().as_str()),
                Some(aterm_toolchain::stall::STALLED_AFTER)
            )
        );

        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let machine = room(&repo, 300);
        let mut said = Vec::new();
        let whole = claim_on(s.clone(), &ledger, &machine, &mut said)
            .expect("admitted")
            .expect("held");
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(whole.record().need.disk, 39 * GIB);
        assert_eq!(
            aterm_capacity::covering_disk(
                &ledger,
                &machine,
                std::process::id(),
                &whole.marker(),
                &repo
            )
            .expect("read")
            .map(|r| r.need.what),
            Some("release 0.156.0".to_owned())
        );
        drop(whole);
        assert!(ledger.list().expect("listed").is_empty());

        let never = claim_on(
            sized(&repo, 500 * GIB, "release 0.156.0", Vec::new()),
            &ledger,
            &machine,
            &mut said,
        )
        .expect_err("refused");
        assert!(!never.could_not_run, "{never:?}");
        assert!(never.why.contains("can never be admitted"), "{never:?}");

        let mut said = Vec::new();
        let unclaimed = answer(
            Err(Error::Ledger(std::io::Error::other("read-only"))),
            &mut |l| said.push(l.to_owned()),
        )
        .expect("goes on");
        assert!(unclaimed.is_none());
        assert_eq!(said.len(), 1, "{said:?}");
    }

    /// A CLAIM WHOSE WAIT ENDED UNDECIDED REFUSES THE RELEASE AS ONE THAT COULD NOT RUN (resumable,
    /// nothing about the code decided): an interrupt that ended its wait, and a wait given up behind
    /// holders that made no progress. NEGATIVE CONTROL: a claim that can never fit is a decided
    /// refusal ([`the_release_claim_is_one_held_disk_claim_its_runs_are_covered_by`]).
    #[test]
    fn a_claim_whose_wait_ended_undecided_could_not_run() {
        let stopped = Error::Stopped {
            signal: Some(2),
            short: None,
        };
        let stuck = Error::Stuck {
            stuck: aterm_toolchain::waits::Stuck {
                holders: vec![4242],
                span: std::time::Duration::from_secs(16 * 60),
            },
            short: aterm_capacity::Shortfall {
                shorts: Vec::new(),
                holders: Vec::new(),
            },
        };
        for e in [stopped, stuck] {
            let said = e.to_string();
            let refused = answer(Err(e), &mut |_| {}).expect_err("refused");
            assert!(refused.could_not_run, "{said}: {refused:?}");
        }
    }

    /// THE RELEASE KEEPS THE BUILD DIRECTORY ITS VERIFY RUNS BUILD IN ([`own_dirs`]): the one the
    /// environment of a release child names for the clone by its physical path, as `verify` reads
    /// it there and as the release's own build beside it is handed
    /// (`crate::release_steps::verify_layout`), whether the clone is named through a link or not,
    /// then the release cache's target root.
    #[cfg(unix)]
    #[test]
    fn the_release_keeps_the_build_directory_its_verify_runs_build_in() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("release-disk-own-")
            .tempdir()
            .expect("scratch");
        let clone = dir.path().join("clone");
        std::fs::create_dir_all(&clone).expect("the clone");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&clone, &link).expect("a link to it");
        let physical = aterm_verify::physical(&clone);
        let build = crate::release_steps::verify_layout(&clone)
            .1
            .and_then(|v| aterm_verify::disk::build_dir_path(&v, &physical))
            .expect("a build directory its verify runs build in");
        assert!(
            build
                .to_string_lossy()
                .contains(&*physical.to_string_lossy()),
            "{} is spelled through the physical clone {}",
            build.display(),
            physical.display()
        );
        let mut own = vec![build];
        own.extend(
            crate::snapshot::CacheRoot::for_repo(&link)
                .ok()
                .map(|c| c.target()),
        );
        assert_eq!(own_dirs(&link), own);
    }
}
