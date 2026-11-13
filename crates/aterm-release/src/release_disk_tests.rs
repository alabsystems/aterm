// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
/// (`crate::release_steps::verify_plan`), whether the clone is named through a link or not,
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
    let plan = crate::release_steps::verify_plan(&clone).expect("the clone's build layout");
    let build = plan
        .build_dir()
        .and_then(|v| aterm_verify::disk::build_dir_path(v, &physical))
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
