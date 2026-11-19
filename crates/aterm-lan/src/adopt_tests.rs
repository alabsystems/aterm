// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt as _;

/// launchd's `ThrottleInterval` where a job sets none, as the broker's plist does.
const LAUNCHD_THROTTLE: Duration = Duration::from_secs(10);

/// The hang detector for a look that must return: never a latency budget.
const MUST_RETURN: Duration = Duration::from_secs(60);

/// The team a test's release names.
const TEAM: &str = "ABCDE12345";

/// The running image's build in these tests.
const RUNNING: u64 = 100;

/// What an image of build `build` holds a candidate to, its tree bound roomy for any test's
/// bundle.
fn bounds(build: u64) -> Bounds {
    Bounds {
        build,
        most: Tree {
            entries: 1000,
            bytes: u64::MAX,
        },
    }
}

/// What a swap retired is this broker's own image: no test image is any process's.
fn ours(_: &Path) -> Option<bool> {
    Some(true)
}

/// A scratch folder at its canonical path: the broker reads a candidate's files with no link on
/// the way, and macOS's temporary folder is reached through one (`/var`).
fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    let root = aterm_tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    (root, base)
}

fn bundle(at: &Path, bytes: &[u8]) {
    let exe = executable_of(at);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, bytes).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o777)).unwrap();
}

/// A team release at `at` stating `build`, its code page `page`, whose executable seals its
/// `Info.plist` ([`install::test_release`]): the executable's bytes.
fn release(at: &Path, build: u64, page: u8) -> Vec<u8> {
    let info = install::test_info(build);
    let exe = install::test_release(TEAM, page, &info);
    bundle(at, &exe);
    std::fs::write(at.join(install::BUNDLE_INFO), &info).unwrap();
    exe
}

fn executable(bundle: &Path) -> Vec<u8> {
    std::fs::read(executable_of(bundle)).unwrap()
}

/// What the folder beside `image` holds: the image alone once no staging is left.
fn beside(image: &Path) -> usize {
    std::fs::read_dir(image.parent().unwrap()).unwrap().count()
}

/// An asker of `uid` whose process loaded the executable now at `bundle`.
fn asker(uid: u32, bundle: &Path) -> Asker {
    Asker {
        uid,
        cdhash: install::test_cdhash(&executable(bundle)).unwrap(),
        bundle: bundle.to_owned(),
    }
}

/// The askers `askers`, newest first, as the server keeps them.
fn asking(askers: &[Asker]) -> Askers {
    let kept = Askers::default();
    for asker in askers.iter().rev() {
        assert!(kept.asked(asker.uid, asker.cdhash, &executable_of(&asker.bundle)));
    }
    kept
}

#[test]
fn a_passing_clone_is_swapped_in_and_a_refused_one_leaves_the_image() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("source.app"), root.join("lan").join("aterm.app"));
    let new = release(&source, RUNNING + 1, 1);
    let old = release(&image, RUNNING, 0);
    let refused = adopt(&source, &image, bounds(RUNNING), &ours, |_| {
        Err(CheckError::Refused("not the team".to_owned()))
    })
    .unwrap_err();
    assert!(matches!(refused, Failure::Check(CheckError::Refused(_))));
    assert_eq!(executable(&image), old);
    let mut judged = None;
    adopt(&source, &image, bounds(RUNNING), &ours, |staged| {
        judged = Some(staged.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(executable(&image), new);
    let judged = judged.unwrap();
    assert_ne!(
        judged, source,
        "the clone is judged, never the person's bundle"
    );
    assert!(!judged.exists(), "the replaced image is gone");
    let mode = std::fs::metadata(executable_of(&image)).unwrap().mode();
    assert_eq!(mode & 0o022, 0, "no group or other write bit: {mode:o}");
    assert_eq!(beside(&image), 1, "no staging left beside the image");
    assert_eq!(executable(&source), new);

    // A temporary staging obstruction is ordinary filesystem failure, not
    // a code refusal or a new source generation. Its repair must be noticed
    // by the next ordinary watch round without restarting the broker.
    let old = release(&image, RUNNING, 0);
    let image_exe = executable_of(&image);
    let original = stamp(&source).unwrap();
    let staging_parent = root.join("unavailable-staging-parent");
    std::fs::write(&staging_parent, b"temporary obstruction").unwrap();
    let mut observation = Adoption::default();
    let now = Cell::new(Duration::ZERO);
    let first = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt_allocating(
                &source,
                &image,
                bounds(RUNNING),
                &ours,
                |_| panic!("failed private allocation precedes code verification"),
                |_| aterm_tempfile::TempDir::new_in(&staging_parent),
                aterm_uds::app_image::clone,
            )
        },
    );
    assert!(matches!(
        first,
        Round::Failed {
            error: Failure::Io(_),
            report: true
        }
    ));
    assert_eq!(std::fs::read(&image_exe).unwrap(), old);
    std::fs::remove_file(&staging_parent).unwrap();
    std::fs::create_dir(&staging_parent).unwrap();
    now.set(LOOK_EVERY);
    let retried = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt_allocating(
                &source,
                &image,
                bounds(RUNNING),
                &ours,
                |_| Ok(()),
                |_| aterm_tempfile::TempDir::new_in(&staging_parent),
                aterm_uds::app_image::clone,
            )
        },
    );
    assert_eq!(
        stamp(&source).unwrap(),
        original,
        "the source did not change"
    );
    assert_eq!(
        std::fs::read(&image_exe).unwrap(),
        new,
        "the ordinary round must retry the same source after transient failure"
    );
    assert!(matches!(retried, Round::Applied));

    // Resource-only repair leaves the executable stamp unchanged. The
    // actual signed bundle becomes admissible again at the existing retry.
    aterm_uds::deadline::require_trust_service();
    let resources = source.join("Contents/Resources");
    std::fs::create_dir_all(&resources).unwrap();
    let resource = resources.join("restored-link");
    std::os::unix::fs::symlink("../MacOS/aterm", &resource).unwrap();
    std::fs::copy("/usr/bin/true", executable_of(&source)).unwrap();
    std::fs::write(source.join("Contents/Info.plist"), b"<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>aterm</string><key>CFBundleIdentifier</key><string>com.aterm.adoption-fixture</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>102</string></dict></plist>").unwrap();
    let signed = aterm_uds::spawnfd::command("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(signed.status.success(), "{signed:?}");
    let identity = Identity::for_code_at(&source).unwrap();
    let signed_stamp = stamp(&source).unwrap();
    std::fs::remove_file(&resource).unwrap();
    std::os::unix::fs::symlink("../../../outside", &resource).unwrap();
    let damaged = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &ours, |candidate| {
                identity.admits_code_at_strictly(candidate)
            })
        },
    );
    assert!(matches!(
        damaged,
        Round::Failed {
            error: Failure::Refused(_),
            ..
        }
    ));
    assert!(!observation.settled);
    assert_eq!(std::fs::read(&image_exe).unwrap(), new);
    std::fs::remove_file(&resource).unwrap();
    std::os::unix::fs::symlink("../MacOS/aterm", &resource).unwrap();
    assert_eq!(stamp(&source).unwrap(), signed_stamp);
    now.set(now.get() + LOOK_EVERY);
    let repaired = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &ours, |candidate| {
                identity.admits_code_at_strictly(candidate)
            })
        },
    );
    assert!(matches!(repaired, Round::Applied));
    assert_eq!(
        std::fs::read(&image_exe).unwrap(),
        std::fs::read(executable_of(&source)).unwrap()
    );

    bundle(&source, b"refused");
    let denied = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &ours, |_| {
                Err(CheckError::Refused("different signer".into()))
            })
        },
    );
    assert!(matches!(
        denied,
        Round::Failed {
            error: Failure::Check(CheckError::Refused(_)),
            report: true
        }
    ));
    for _ in 0..20 {
        now.set(now.get() + LOOK_EVERY);
        assert!(matches!(
            observation.round(
                || now.get(),
                &source,
                &image,
                RUNNING,
                || panic!("unchanged refused source must not clone or reverify")
            ),
            Round::Quiet
        ));
    }
    bundle(&source, b"retry");
    let last_attempt = Cell::new(None);
    let intervals = RefCell::new(Vec::new());
    let mut reports = 0;
    for _ in 0..600 {
        let result = observation.round(
            || now.get(),
            &source,
            &image,
            RUNNING,
            || {
                adopt(&source, &image, bounds(RUNNING), &ours, |_| {
                    if let Some(last) = last_attempt.replace(Some(now.get())) {
                        intervals.borrow_mut().push(now.get() - last);
                    }
                    Err(CheckError::Unanswered(
                        "the strict check is temporarily unavailable".into(),
                    ))
                })
            },
        );
        reports += usize::from(matches!(result, Round::Failed { report: true, .. }));
        now.set(now.get() + LOOK_EVERY);
        if intervals
            .borrow()
            .ends_with(&[LONGEST_RETRY, LONGEST_RETRY])
        {
            break;
        }
    }
    assert_eq!(
        reports, 1,
        "one unchanged operational diagnostic is not logged every round"
    );
    assert_eq!(intervals.borrow().first(), Some(&LOOK_EVERY));
    assert_eq!(intervals.borrow().get(1), Some(&(LOOK_EVERY * 2)));
    assert!(
        intervals
            .borrow()
            .ends_with(&[LONGEST_RETRY, LONGEST_RETRY]),
        "operational retry remains live at its bounded rate"
    );
    assert!(
        intervals
            .borrow()
            .iter()
            .all(|interval| *interval <= LONGEST_RETRY)
    );
    let pending = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || panic!("the same retry is not due"),
    );
    assert!(matches!(pending, Round::Quiet));
    let bypassing = release(&source, RUNNING + 3, 3);
    assert!(matches!(
        observation.round(
            || now.get(),
            &source,
            &image,
            RUNNING,
            || adopt(&source, &image, bounds(RUNNING), &ours, |_| Ok(()))
        ),
        Round::Applied
    ));
    assert_eq!(
        std::fs::read(&image_exe).unwrap(),
        bypassing,
        "a new source bypasses the old backoff"
    );

    bundle(&source, b"changing");
    let changed = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &ours, |_| {
                release(&source, RUNNING + 4, 4);
                Err(CheckError::Refused("the old candidate was refused".into()))
            })
        },
    );
    assert!(matches!(changed, Round::Failed { .. }));
    assert!(
        !observation.settled,
        "a refusal from a changed source cannot consume its newer generation"
    );
    assert!(matches!(
        observation.round(
            || now.get(),
            &source,
            &image,
            RUNNING,
            || adopt(&source, &image, bounds(RUNNING), &ours, |_| Ok(()))
        ),
        Round::Applied
    ));
}

#[test]
fn a_source_that_is_a_link_or_holds_one_out_of_the_bundle_is_never_adopted() {
    let (_root, root) = scratch();
    let (real, link, image) = (
        root.join("real.app"),
        root.join("link.app"),
        root.join("lan").join("aterm.app"),
    );
    let new = release(&real, RUNNING + 1, 1);
    let old = release(&image, RUNNING, 0);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut judged = false;
    adopt(&link, &image, bounds(RUNNING), &ours, |_| {
        judged = true;
        Ok(())
    })
    .unwrap_err();
    assert!(!judged, "a link is refused before anything is judged");
    assert_eq!(executable(&image), old);
    assert!(
        !std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the image is still a real folder"
    );

    let resources = real.join("Contents").join("Resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::os::unix::fs::symlink("../MacOS/aterm", resources.join("inside")).unwrap();
    adopt(&real, &image, bounds(RUNNING), &ours, |_| Ok(())).unwrap();
    assert_eq!(executable(&image), new);

    for out in ["../../../outside", "/Users//someone/evil", "../x/../../.."] {
        let escape = resources.join("escape");
        let _ = std::fs::remove_file(&escape);
        std::os::unix::fs::symlink(out, &escape).unwrap();
        release(&real, RUNNING + 2, 2);
        let refused = adopt(&real, &image, bounds(RUNNING), &ours, |_| Ok(())).unwrap_err();
        assert!(matches!(refused, Failure::Refused(_)), "{out}");
        assert_eq!(executable(&image), new, "{out}");
    }
    assert_eq!(beside(&image), 1, "no staging left beside the image");
}

/// The short bound an account gives this broker's adoption where it recorded a definite failure
/// (`aterm_uds::lan::adoption::LAPSE`) outlasts the slowest adoption it makes that succeeds after
/// one check with no verdict, computed from its own constants: the look that
/// finds the changed source, a period late; a strict check that gives no verdict at its bound
/// (`aterm_keeper::identity::CODE_CHECK_HANG`); the backoff after that failure, which the look
/// that tries again overruns by up to a period; a second check that answers just inside its
/// bound; and launchd's start of the broker after it exits, within the default throttle its
/// plist leaves in force. MUTANT (run): the bound's former 24 looks, two minutes, fail it.
#[test]
fn an_accounts_bound_outlasts_the_slowest_adoption_that_succeeds() {
    use aterm_keeper::identity::CODE_CHECK_HANG;
    let plist = aterm_uds::lan::install::plist_text(Path::new("/Applications/aterm.app"));
    assert!(!plist.contains("ThrottleInterval"), "{plist}");
    assert_eq!(LOOK_EVERY, aterm_uds::lan::ADOPTION_LOOK);
    let slowest = LOOK_EVERY
        + CODE_CHECK_HANG
        + retry_after(1)
        + LOOK_EVERY
        + CODE_CHECK_HANG
        + LAUNCHD_THROTTLE;
    assert!(
        aterm_uds::lan::adoption::LAPSE >= slowest,
        "{:?} < {slowest:?}",
        aterm_uds::lan::adoption::LAPSE
    );
}

/// Through a `trustd` stall of `aterm_uds::lan::adoption::STALL` that began with the first check,
/// the slowest adoption this broker makes that succeeds ends inside the long bound an account
/// gives one with no definite failure (`aterm_uds::lan::adoption::LONG_LAPSE`), walked with the
/// broker's own backoff ([`retry_after`]) and check bound: the look that finds the source; a check
/// that gives no verdict at its bound, whose helper leaves the adoption's check gate overdue
/// until the stall ends, so no look clones before then ([`Looks::look`]); the first look after
/// both the stall and the backoff, up to a look late; a check that answers just inside its bound;
/// and launchd's start of the broker. MUTANT (run): a long bound of the short one's 36 looks
/// fails it.
#[test]
fn the_long_bound_outlasts_an_adoption_through_a_trustd_stall() {
    use aterm_keeper::identity::CODE_CHECK_HANG;
    use aterm_uds::lan::adoption::{LONG_LAPSE, STALL};
    let first_check = LOOK_EVERY;
    let stall_ends = first_check + STALL;
    let backed_off = first_check + CODE_CHECK_HANG + retry_after(1);
    let again = backed_off.max(stall_ends) + LOOK_EVERY;
    let slowest = again + CODE_CHECK_HANG + LAUNCHD_THROTTLE;
    assert!(LONG_LAPSE >= slowest, "{LONG_LAPSE:?} < {slowest:?}");
}

/// A running broker swaps a clone only for an image there: with none (root removed it), nothing is
/// published and no staging is left, as nothing it runs from could be retired. An incomplete
/// source is never cloned. NEGATIVE CONTROL: the same adoption over an image swaps the clone in.
#[test]
fn with_no_image_an_adoption_publishes_nothing() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("source.app"), root.join("lan").join("aterm.app"));
    let first = release(&source, RUNNING + 1, 1);
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    let none = adopt(&source, &image, bounds(RUNNING), &ours, |_| Ok(())).unwrap_err();
    assert!(
        matches!(&none, Failure::Io(error) if error.kind() == io::ErrorKind::NotFound),
        "{none}"
    );
    assert!(!image.exists());
    assert_eq!(beside(&image), 0, "no staging left");
    release(&image, RUNNING, 0);
    adopt(&source, &image, bounds(RUNNING), &ours, |_| Ok(())).unwrap();
    assert_eq!(executable(&image), first);
    assert!(same_bytes(&executable_of(&source), &executable_of(&image)).unwrap());
    bundle(&source, b"other");
    assert!(!same_bytes(&executable_of(&source), &executable_of(&image)).unwrap());
    std::fs::remove_file(executable_of(&source)).unwrap();
    std::fs::create_dir(executable_of(&source)).unwrap();
    assert!(matches!(
        Adoption::default().round(
            || Duration::ZERO,
            &source,
            &image,
            RUNNING,
            || panic!("an incomplete source must not be cloned or checked")
        ),
        Round::Failed {
            error: Failure::Io(_),
            ..
        }
    ));
}

/// Where a round does not adopt its source the broker says why, in the record an account reads
/// ([`aterm_uds::lan::adoption::Refusal`]): its source, its own build, whether the failure was
/// definite and the reason, for a bundle the strict check refused and for a clone another volume
/// fails (`EXDEV`, which the broker does not copy), both definite, and for a check `trustd` did
/// not answer, which gave no verdict; an adoption that succeeds removes it, and so does an image
/// already the source, only where the record names that source, and that bundle's own asker
/// record: another asker's reason stands.
/// MUTANTS (run): a record not written on `EXDEV` leaves the check's old reason standing and fails
/// the clone's case; one that calls every failure definite fails the unanswered check's.
/// NEGATIVE CONTROL: a failure the last round already recorded writes nothing again.
#[test]
fn a_round_that_does_not_adopt_records_why_and_an_adoption_clears_it() {
    use aterm_uds::lan::adoption::{Kind, Refusal};
    let (_root, root) = scratch();
    let (source, image, record) = (
        root.join("source.app"),
        root.join("lan").join("aterm.app"),
        root.join("lan-adoption.refused"),
    );
    let askers = root.join("lan-adoption.refused.d");
    let refusals = Refusals {
        path: &record,
        source: &source,
    };
    let asking = AskerRefusals { folder: &askers };
    let recorded = || Refusal::parse(&std::fs::read(&record).unwrap()).unwrap();
    release(&source, RUNNING + 1, 1);
    let old = release(&image, RUNNING, 0);
    let mut observation = Adoption::default();
    let now = Cell::new(Duration::ZERO);

    let refused = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &ours, |_| {
                Err(CheckError::Refused("not the team".into()))
            })
        },
    );
    refusals.after(&refused);
    let written = recorded();
    assert_eq!(written.source, source);
    assert_eq!(written.build, aterm_uds::lan::hello_build());
    assert_eq!(written.kind, Kind::Definite);
    assert!(
        written.reason.contains("strict check refused") && written.reason.contains("not the team"),
        "{written:?}"
    );

    // Another source generation, which a clone across volumes fails.
    release(&source, RUNNING + 2, 2);
    let exdev = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt_allocating(
                &source,
                &image,
                bounds(RUNNING),
                &ours,
                |_| panic!("nothing was cloned to check"),
                |parent| aterm_tempfile::TempDir::new_in(parent),
                |_, _| {
                    Err(aterm_uds::app_image::Error::Io(
                        io::Error::from_raw_os_error(libc::EXDEV),
                    ))
                },
            )
        },
    );
    assert!(
        matches!(&exdev, Round::Failed { error: Failure::Clone(error), report: true }
            if error.raw_os_error() == Some(libc::EXDEV)),
        "the errno is kept"
    );
    refusals.after(&exdev);
    let written = recorded();
    assert_eq!(written.kind, Kind::Definite);
    assert!(
        written.reason.contains("another volume")
            && written
                .reason
                .contains(&format!("os error {}", libc::EXDEV)),
        "{written:?}"
    );
    assert_eq!(executable(&image), old);

    // The same failure again records nothing anew.
    std::fs::remove_file(&record).unwrap();
    now.set(LOOK_EVERY);
    let again = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || {
            adopt_allocating(
                &source,
                &image,
                bounds(RUNNING),
                &ours,
                |_| panic!("nothing was cloned to check"),
                |parent| aterm_tempfile::TempDir::new_in(parent),
                |_, _| {
                    Err(aterm_uds::app_image::Error::Io(
                        io::Error::from_raw_os_error(libc::EXDEV),
                    ))
                },
            )
        },
    );
    assert!(matches!(again, Round::Failed { report: false, .. }));
    refusals.after(&again);
    assert!(!record.exists());

    // An adoption that succeeds removes the record.
    refusals.after(&exdev);
    assert!(record.exists());
    release(&source, RUNNING + 3, 3);
    let applied = observation.round(
        || now.get(),
        &source,
        &image,
        RUNNING,
        || adopt(&source, &image, bounds(RUNNING), &ours, |_| Ok(())),
    );
    assert!(matches!(applied, Round::Applied));
    refusals.after(&applied);
    assert!(!record.exists(), "an adoption clears the record");

    // An image already the source leaves every asker's record standing, and an asker's round
    // that finds the image is its bundle clears that bundle's own.
    let asked = root.join("asker.app");
    let not_newer = || Round::Failed {
        error: Failure::NotNewer("its build 1 is not newer".into()),
        report: true,
    };
    asking.after(&asked, &not_newer());
    asking.after(&source, &not_newer());
    let asker_record = |bundle: &Path| aterm_uds::lan::adoption::asker_refusal_in(&askers, bundle);
    assert!(!record.exists(), "an asker's record is its own");
    assert!(asker_record(&asked).exists() && asker_record(&source).exists());
    let current = || {
        Adoption::default().round(
            || Duration::ZERO,
            &source,
            &image,
            RUNNING,
            || panic!("an image that is the source is not adopted again"),
        )
    };
    let current_round = current();
    assert!(matches!(current_round, Round::Current));
    refusals.after(&current_round);
    assert!(asker_record(&asked).exists() && asker_record(&source).exists());
    asking.after(&source, &current_round);
    assert!(
        asker_record(&asked).exists(),
        "another asker's reason stands"
    );
    assert!(
        !asker_record(&source).exists(),
        "the image's own is cleared"
    );

    // It clears the source's own.
    refusals.after(&refused);
    assert!(record.exists());
    refusals.after(&current());
    assert!(!record.exists(), "an image that is the source clears it");

    // A check `trustd` did not answer gave no verdict, and its record says so.
    bundle(&source, b"newer still");
    let unanswered = Adoption::default().round(
        || Duration::ZERO,
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &ours, |_| {
                Err(CheckError::Unanswered("trustd did not answer".into()))
            })
        },
    );
    refusals.after(&unanswered);
    let written = recorded();
    assert_eq!(written.kind, Kind::NoVerdict, "{written:?}");
    assert!(written.reason.contains("no verdict"), "{written:?}");
}

/// The clone's own files decide, after its strict check passed: a build equal to or older than the
/// running image's, an executable that binds no `Info.plist`, and a clone whose staged files state
/// an older build than the source's did, are each refused, definite, the image left as it was and
/// no staging left; a newer one is adopted. NEGATIVE CONTROL (run): without `newer_than` the
/// older clone is adopted.
#[test]
fn an_older_or_equal_build_is_never_adopted_though_its_check_passes() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("source.app"), root.join("lan").join("aterm.app"));
    let running = 120;
    let old = release(&image, running, 0);
    for build in [100, 120] {
        release(&source, build, 1);
        let refused = adopt(&source, &image, bounds(running), &ours, |_| Ok(())).unwrap_err();
        assert!(
            matches!(&refused, Failure::NotNewer(why)
                if why == &format!("its build {build} is not newer than the running image's 120: the broker never moves to an older build")),
            "{build}: {refused}"
        );
        assert_eq!(refused.kind(), aterm_uds::lan::adoption::Kind::Definite);
        assert_eq!(executable(&image), old, "{build}");
        assert_eq!(beside(&image), 1, "{build}: no staging left");
    }

    // An executable that binds no Info.plist states no build, whatever its plist says.
    #[cfg(target_arch = "x86_64")]
    let cpu = 0x0100_0007;
    #[cfg(not(target_arch = "x86_64"))]
    let cpu = 0x0100_000c;
    let unbound = install::test_directory(install::BUNDLE_ID, Some(TEAM), 0, 2, None);
    bundle(&source, &install::test_image(cpu, &[unbound.as_slice()]));
    std::fs::write(source.join(install::BUNDLE_INFO), install::test_info(130)).unwrap();
    let refused = adopt(&source, &image, bounds(running), &ours, |_| Ok(())).unwrap_err();
    assert!(
        matches!(&refused, Failure::NotNewer(why) if why.contains("does not seal its Info.plist")),
        "{refused}"
    );
    assert_eq!(executable(&image), old);

    let new = release(&source, 121, 3);
    adopt(&source, &image, bounds(running), &ours, |_| Ok(())).unwrap();
    assert_eq!(executable(&image), new);

    // The staged clone holds an older build than the source said: its own number decides.
    release(&source, 122, 4);
    let refused = adopt_allocating(
        &source,
        &image,
        bounds(running),
        &ours,
        |_| Ok(()),
        |parent| aterm_tempfile::TempDir::new_in(parent),
        |from, to| {
            aterm_uds::app_image::clone(from, to)?;
            release(to, 100, 5);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(
        matches!(&refused, Failure::NotNewer(why) if why.contains("its build 100 is not newer")),
        "{refused}"
    );
    assert_eq!(executable(&image), new);
    assert_eq!(beside(&image), 1, "no staging left");
}

/// A source that went away or is older pins nothing: the source lane is quiet, records nothing
/// and clones nothing, and the asker lane adopts the newest authenticated asker, one clone a look.
/// NEGATIVE CONTROL (run): with the asker lane left out, nothing is ever adopted.
#[test]
fn an_abandoned_or_older_source_does_not_pin_the_image() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (lower, higher) = (root.join("lower.app"), root.join("higher.app"));
    release(&lower, RUNNING + 5, 5);
    release(&higher, RUNNING + 10, 6);
    let tried = RefCell::new(Vec::new());
    let adopt = |bundle: &Path| -> Result<(), Failure> {
        tried.borrow_mut().push(bundle.to_owned());
        Ok(())
    };
    let mut looks = Looks::new(bounds(RUNNING));
    let none = Askers::default();

    let (round, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &none, adopt);
    assert!(matches!(round, Round::Quiet), "a missing source is quiet");
    assert!(asked.is_none());
    release(&source, RUNNING - 10, 1);
    for _ in 0..2 {
        let (round, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &none, adopt);
        assert!(
            matches!(round, Round::Quiet),
            "an older source is passed over"
        );
        assert!(asked.is_none());
    }
    assert!(tried.borrow().is_empty(), "nothing was cloned");

    let askers = asking(&[asker(501, &lower), asker(502, &higher)]);
    let (round, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &askers, adopt);
    assert!(matches!(round, Round::Quiet));
    let (bundle, asked) = asked.expect("the asker lane adopts");
    assert!(matches!(asked, Round::Applied));
    assert_eq!(bundle, higher);
    assert_eq!(*tried.borrow(), vec![higher], "one clone, the newest");
}

/// An asker's number orders nothing until its executable's loaded CodeDirectory seals it: an
/// asker whose plist says 999 under a slot that seals another is never cloned and is forgotten,
/// one whose loaded code is not on disk likewise, and neither keeps the source from being adopted
/// on its look. NEGATIVE CONTROL (run): without the slot's seal the forged asker is cloned.
#[test]
fn a_forged_asker_number_is_never_cloned_and_does_not_starve_the_source() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("source.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (forged, genuine, swapped) = (
        root.join("forged.app"),
        root.join("genuine.app"),
        root.join("swapped.app"),
    );
    release(&forged, RUNNING + 5, 7);
    let forged_asker = asker(501, &forged);
    std::fs::write(forged.join(install::BUNDLE_INFO), install::test_info(999)).unwrap();
    release(&swapped, RUNNING + 6, 8);
    let swapped_asker = asker(503, &swapped);
    release(&swapped, 998, 9);
    release(&genuine, RUNNING + 10, 10);
    let askers = asking(&[
        forged_asker.clone(),
        swapped_asker.clone(),
        asker(502, &genuine),
    ]);
    let tried = RefCell::new(Vec::new());
    let adopt = |bundle: &Path| -> Result<(), Failure> {
        tried.borrow_mut().push(bundle.to_owned());
        Ok(())
    };
    let mut looks = Looks::new(bounds(RUNNING));

    release(&source, RUNNING + 10, 1);
    let (round, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &askers, adopt);
    assert!(matches!(round, Round::Applied), "the source is adopted");
    assert!(asked.is_none(), "one clone a look");
    let (round, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &askers, adopt);
    assert!(matches!(round, Round::Quiet));
    let (bundle, _) = asked.expect("the genuine asker is adopted");
    assert_eq!(bundle, genuine);
    assert_eq!(*tried.borrow(), vec![source, genuine]);
    let kept = askers.now();
    assert!(!kept.contains(&forged_asker) && !kept.contains(&swapped_asker));
}

/// A uid whose asker's clone failed waits its backoff whatever it changes: the same bundle asking
/// again with a new executable, or another bundle of that uid, clones nothing before
/// `retry_after(1)`, while another uid's asker is cloned; after it, that uid is tried again.
/// NEGATIVE CONTROL (run): without the backoff skip the uid's new bundle is cloned at once.
#[test]
fn a_refused_askers_uid_backs_off_across_stamps_and_paths() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (theirs, refused, other) = (
        root.join("theirs.app"),
        root.join("refused.app"),
        root.join("other.app"),
    );
    release(&theirs, RUNNING + 5, 1);
    release(&refused, RUNNING + 10, 2);
    let askers = asking(&[asker(502, &refused), asker(501, &theirs)]);
    let tried = RefCell::new(Vec::new());
    let adopt = |bundle: &Path| -> Result<(), Failure> {
        tried.borrow_mut().push(bundle.to_owned());
        if bundle == refused {
            Err(Failure::Check(CheckError::Refused("not the team".into())))
        } else {
            Ok(())
        }
    };
    let now = Cell::new(Duration::ZERO);
    let mut looks = Looks::new(bounds(RUNNING));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert!(matches!(
        asked,
        Some((_, Round::Failed { report: true, .. }))
    ));

    release(&refused, RUNNING + 11, 3);
    let again = asker(502, &refused);
    assert!(askers.asked(again.uid, again.cdhash, &executable_of(&refused)));
    release(&other, RUNNING + 12, 4);
    let elsewhere = asker(502, &other);
    assert!(askers.asked(elsewhere.uid, elsewhere.cdhash, &executable_of(&other)));
    now.set(retry_after(1) - Duration::from_millis(1));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    let (bundle, round) = asked.expect("the other uid's asker is adopted");
    assert!(matches!(round, Round::Applied));
    assert_eq!(bundle, theirs);

    now.set(retry_after(1));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert_eq!(asked.map(|(bundle, _)| bundle), Some(other.clone()));
    assert_eq!(*tried.borrow(), vec![refused.clone(), theirs, other]);
}

/// A swap whose retired image is not this broker's own (an elevation's install published one
/// between the check and the swap) is undone: the elevation's image stays, no staging is left, and
/// the round asks the broker to exit for launchd. Where what it retired cannot be told, the swap is
/// undone too. NEGATIVE CONTROL (run): without the check of what the swap retired, the clone
/// replaces the elevation's image.
#[test]
fn a_swap_over_an_elevations_image_is_undone() {
    let (_root, root) = scratch();
    let (source, image, elevation) = (
        root.join("source.app"),
        root.join("lan").join("aterm.app"),
        root.join("elevation.app"),
    );
    release(&source, RUNNING + 10, 1);
    release(&image, RUNNING, 0);
    let installed = release(&elevation, RUNNING + 5, 5);
    let loaded = std::fs::metadata(executable_of(&image)).unwrap().ino();
    let own = move |exe: &Path| -> Option<bool> {
        Some(std::fs::symlink_metadata(exe).ok()?.ino() == loaded)
    };
    let round = Adoption::default().round(
        || Duration::ZERO,
        &source,
        &image,
        RUNNING,
        || {
            adopt(&source, &image, bounds(RUNNING), &own, |_| {
                std::fs::remove_dir_all(&image).unwrap();
                std::fs::rename(&elevation, &image).unwrap();
                Ok(())
            })
        },
    );
    assert!(
        matches!(
            round,
            Round::Failed {
                error: Failure::Superseded,
                report: true
            }
        ),
        "the round asks for an exit"
    );
    assert_eq!(executable(&image), installed, "the elevation's image stays");
    assert_eq!(beside(&image), 1, "no staging left");

    let before = executable(&image);
    let unknown = adopt(&source, &image, bounds(RUNNING), &|_| None, |_| Ok(())).unwrap_err();
    assert!(matches!(unknown, Failure::Io(_)), "{unknown}");
    assert_eq!(executable(&image), before, "an unknown retiree is put back");
    assert_eq!(beside(&image), 1, "no staging left");
}

/// An image whose `Info.plist` states no build runs as build 0, so a candidate that states one is
/// adopted as before the version rule. NEGATIVE CONTROL (run): an image with no number taken as
/// the highest refuses it.
#[test]
fn a_running_image_with_no_number_adopts_as_today() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("source.app"), root.join("lan").join("aterm.app"));
    bundle(&image, b"an image from before the rule");
    assert_eq!(running_build(&image), 0, "no Info.plist");
    std::fs::write(
        image.join(install::BUNDLE_INFO),
        b"<plist version=\"1.0\"><dict></dict></plist>",
    )
    .unwrap();
    assert_eq!(running_build(&image), 0, "no CFBundleVersion");
    let new = release(&source, 5, 1);
    adopt(
        &source,
        &image,
        bounds(running_build(&image)),
        &ours,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(executable(&image), new);
    assert_eq!(running_build(&image), 5);
}

/// The candidate reads follow no link and never wait on a FIFO: a source whose `Contents` is a
/// link to an older release states nothing to the hint (whose `None` only skips nothing), an
/// asker reached through such a link is never cloned and is forgotten, and a look over a source
/// and an asker whose `Info.plist` is a FIFO returns, under a one-minute hang detector.
/// NEGATIVE CONTROL (run): opened without `O_NONBLOCK` the read waits on the FIFO and the
/// detector fires.
#[test]
fn the_hint_read_follows_no_link_and_never_blocks() {
    let (_root, root) = scratch();
    let (source, image, older) = (
        root.join("source.app"),
        root.join("lan").join("aterm.app"),
        root.join("older.app"),
    );
    release(&image, RUNNING, 0);
    release(&older, RUNNING - 10, 9);
    std::fs::create_dir_all(&source).unwrap();
    std::os::unix::fs::symlink(older.join("Contents"), source.join("Contents")).unwrap();
    assert_eq!(stated_build(&older), Some(RUNNING - 10));
    assert_eq!(stated_build(&source), None, "no link is followed");

    let (linked, real) = (root.join("linked.app"), root.join("real.app"));
    release(&real, RUNNING + 10, 3);
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(real.join("Contents"), linked.join("Contents")).unwrap();
    let askers = asking(&[asker(501, &linked)]);
    let mut looks = Looks::new(bounds(RUNNING));
    let gone = root.join("gone.app");
    let (_, asked) = looks.look(
        || Duration::ZERO,
        false,
        &gone,
        &image,
        &askers,
        |_| panic!("an asker through a link is not cloned"),
    );
    assert!(asked.is_none());
    assert!(askers.now().is_empty(), "it is forgotten");

    let (fifo_source, fifo_asker) = (root.join("fifo-source.app"), root.join("fifo-asker.app"));
    for bundle in [&fifo_source, &fifo_asker] {
        release(bundle, RUNNING + 10, 4);
    }
    let asked_fifo = asker(502, &fifo_asker);
    let fifo_source_seen = fifo_source.clone();
    for bundle in [&fifo_source, &fifo_asker] {
        let plist = bundle.join(install::BUNDLE_INFO);
        std::fs::remove_file(&plist).unwrap();
        let path = std::ffi::CString::new(plist.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path live for the call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o644) }, 0);
    }
    let (done, finished) = std::sync::mpsc::channel();
    let image_looked = image.clone();
    std::thread::spawn(move || {
        let askers = asking(&[asked_fifo]);
        let mut looks = Looks::new(bounds(RUNNING));
        let tried = RefCell::new(Vec::new());
        let adopt = |bundle: &Path| -> Result<(), Failure> {
            tried.borrow_mut().push(bundle.to_owned());
            Err(Failure::Refused("a fake adoption".into()))
        };
        // The source's look clones (its hint said nothing); the next, with the source backed
        // off, looks at the asker.
        let (_, first) = looks.look(
            || Duration::ZERO,
            false,
            &fifo_source,
            &image_looked,
            &askers,
            adopt,
        );
        let (_, second) = looks.look(
            || Duration::ZERO,
            false,
            &fifo_source,
            &image_looked,
            &askers,
            adopt,
        );
        let unasked = first.is_none() && second.is_none();
        let _ = done.send((tried.into_inner(), unasked, askers.now().is_empty()));
    });
    let (tried, unasked, forgotten) = finished
        .recv_timeout(MUST_RETURN)
        .expect("a look over a FIFO returns");
    assert_eq!(
        tried,
        vec![fifo_source_seen],
        "the source's hint skips nothing"
    );
    assert!(unasked && forgotten, "the FIFO asker is never cloned");
}

/// The bound on root's work holds on the clone, where no account can change it: an asker whose
/// bundle was small when it was authenticated and grew before its clone (a folder moved in, one
/// large file) is refused at its clone before anything seals or checks it, the image left as it
/// was and no staging left; a clone at the bound exactly is adopted. Entries and bytes are each
/// bounded. NEGATIVE CONTROL (run): without the count of the clone, the grown bundle reaches its
/// check.
#[test]
fn a_clone_past_the_bound_is_refused_before_its_seal_and_check() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    let old = release(&image, RUNNING, 0);
    let asked = root.join("asked.app");
    let new = release(&asked, RUNNING + 10, 1);
    let exact = tree_within(
        &asked,
        Tree {
            entries: u64::MAX,
            bytes: u64::MAX,
        },
        &aterm_sysprobe::volumes::table(),
    )
    .unwrap()
    .unwrap();
    for (what, most, grow) in [
        (
            "entries",
            exact,
            Box::new(|bundle: &Path| {
                std::fs::create_dir(bundle.join("Contents/Moved")).unwrap();
            }) as Box<dyn Fn(&Path)>,
        ),
        (
            "bytes",
            Tree {
                entries: exact.entries + 10,
                bytes: exact.bytes,
            },
            Box::new(|bundle: &Path| {
                std::fs::write(bundle.join("Contents/large"), b"one byte past").unwrap();
            }),
        ),
    ] {
        let bounds = Bounds {
            build: RUNNING,
            most,
        };
        let askers = asking(&[asker(501, &asked)]);
        let mut looks = Looks::new(bounds);
        let (_, round) = looks.look(
            || Duration::ZERO,
            false,
            &source,
            &image,
            &askers,
            |bundle| {
                grow(bundle);
                adopt(bundle, &image, bounds, &ours, |_| {
                    panic!("{what}: a clone past the bound is never checked")
                })
            },
        );
        let (_, round) = round.expect("the small bundle authenticated");
        assert!(
            matches!(&round, Round::Failed { error: Failure::TooLarge(why), .. }
                if why.contains("its clone holds more than")),
            "{what}"
        );
        assert_eq!(executable(&image), old, "{what}");
        assert_eq!(beside(&image), 1, "{what}: no staging left");
        let _ = std::fs::remove_dir(asked.join("Contents/Moved"));
        let _ = std::fs::remove_file(asked.join("Contents/large"));
    }
    adopt(
        &asked,
        &image,
        Bounds {
            build: RUNNING,
            most: exact,
        },
        &ours,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(executable(&image), new, "a clone at the bound is adopted");
}

/// A clone's strict check runs through the adoption's own gate: one adoption check overdue turns
/// the next adoption check away with no verdict, while the process's `trustd` gate, which every
/// admission's peer check runs through, still starts checks. NEGATIVE CONTROL (run): the strict
/// check through the `trustd` gate gives a verdict past the overdue adoption check.
#[test]
fn an_overdue_adoption_check_never_turns_an_admission_away() {
    let (_root, root) = scratch();
    let (release_it, held) = std::sync::mpsc::channel::<()>();
    let overdue = ADOPTION_CHECKS
        .start(Duration::ZERO, move || {
            let _ = held.recv();
        })
        .unwrap();
    let started = std::time::Instant::now();
    while std::time::Instant::now() <= started {
        aterm_uds::deadline::poll_yield();
    }
    let checker = Identity::for_requirement("anchor apple generic").unwrap();
    let refused = strictly(&checker, &root).unwrap_err();
    assert!(matches!(refused, CheckError::Unanswered(_)), "{refused:?}");
    let admission = aterm_uds::deadline::TRUST
        .start(MUST_RETURN, || "checked")
        .expect("the trustd gate is not overdue");
    assert_eq!(admission.wait(MUST_RETURN).ok(), Some(Ok("checked")));
    drop(release_it);
    assert!(overdue.wait(MUST_RETURN).is_ok(), "the held check ends");
}

/// A uid whose asker's check got no answer waits the longest retry at once, not one look: its
/// gate stays overdue until that check ends, and a try before then clones and seals the bundle
/// for nothing. NEGATIVE CONTROL (run): with the ordinary backoff, the uid is tried again a look
/// later.
#[test]
fn an_unanswered_askers_uid_waits_the_longest_retry() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let slow = root.join("slow.app");
    release(&slow, RUNNING + 10, 1);
    let askers = asking(&[asker(501, &slow)]);
    let tried = Cell::new(0);
    let adopt = |_: &Path| -> Result<(), Failure> {
        tried.set(tried.get() + 1);
        Err(Failure::Check(CheckError::Unanswered(
            "trustd did not answer".into(),
        )))
    };
    let now = Cell::new(Duration::ZERO);
    let mut looks = Looks::new(bounds(RUNNING));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert!(matches!(asked, Some((_, Round::Failed { .. }))));
    for at in [retry_after(1), LONGEST_RETRY - Duration::from_millis(1)] {
        now.set(at);
        let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
        assert!(asked.is_none(), "{at:?}");
    }
    now.set(LONGEST_RETRY);
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert!(asked.is_some());
    assert_eq!(tried.get(), 2);
}

/// An asker whose clone failed for good (its check refused its code, or it is on another volume)
/// is never cloned again, even where its bundle asks again with the same code, so once its uid's
/// backoff ends that uid's next asker is. A clone that failed otherwise is tried again.
/// NEGATIVE CONTROL (run): with nothing remembered, the refused asker is cloned again at every
/// backoff and the good one never.
#[test]
fn a_bundle_refused_for_good_does_not_stand_before_its_uids_next() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (refused, good, other, flaky) = (
        root.join("refused.app"),
        root.join("good.app"),
        root.join("other.app"),
        root.join("flaky.app"),
    );
    release(&refused, RUNNING + 30, 1);
    release(&good, RUNNING + 25, 2);
    release(&other, RUNNING + 40, 3);
    release(&flaky, RUNNING + 50, 4);
    let askers = asking(&[asker(501, &refused), asker(501, &good)]);
    let tried = RefCell::new(Vec::new());
    let adopt = |bundle: &Path| -> Result<(), Failure> {
        tried.borrow_mut().push(bundle.to_owned());
        if bundle == refused {
            Err(Failure::Check(CheckError::Refused(
                "OSStatus -67050: the code does not satisfy its requirement".into(),
            )))
        } else if bundle == other {
            Err(Failure::Clone(io::Error::from_raw_os_error(libc::EXDEV)))
        } else if bundle == flaky {
            Err(Failure::Clone(io::Error::from_raw_os_error(libc::EIO)))
        } else {
            Ok(())
        }
    };
    let now = Cell::new(Duration::ZERO);
    let mut looks = Looks::new(bounds(RUNNING));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert_eq!(asked.map(|(bundle, _)| bundle), Some(refused.clone()));
    let again = asker(501, &refused);
    assert!(askers.asked(again.uid, again.cdhash, &executable_of(&refused)));
    now.set(retry_after(1));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    let (bundle, round) = asked.expect("the uid's next asker");
    assert_eq!(bundle, good);
    assert!(matches!(round, Round::Applied));
    assert!(
        !askers.now().contains(&again),
        "the refused asker is forgotten"
    );

    let askers = asking(&[asker(502, &other), asker(503, &flaky)]);
    let (_, first) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    let (_, second) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert_eq!(
        [first, second].map(|asked| asked.map(|(bundle, _)| bundle)),
        [Some(flaky.clone()), Some(other.clone())]
    );
    now.set(retry_after(1) * 4);
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert_eq!(
        asked.map(|(bundle, _)| bundle),
        Some(flaky.clone()),
        "a clone that failed otherwise is tried again"
    );
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert!(asked.is_none(), "another volume is for good");
    assert_eq!(
        *tried.borrow(),
        vec![refused, good, flaky.clone(), other, flaky]
    );
}

/// A damaged copy of a newer build (a sealed resource edited or missing, which the framework
/// reports with no verdict: `OSStatus -67054`) fails only its own candidacy: once its uid's
/// backoff ends, that uid's next asker, of an older build that never failed, is adopted, and the
/// damaged one stays, tried again where it is the only one. NEGATIVE CONTROL (run): ranked by
/// build alone, the damaged copy is cloned at every backoff and the older good one never.
#[test]
fn a_damaged_copy_goes_behind_its_uids_next_and_stays_to_be_tried_again() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (damaged, good) = (root.join("Downloads/aterm.app"), root.join("aterm.app"));
    release(&damaged, RUNNING + 30, 1);
    release(&good, RUNNING + 25, 2);
    let askers = asking(&[asker(501, &damaged), asker(501, &good)]);
    let tried = RefCell::new(Vec::new());
    let adopt = |bundle: &Path| -> Result<(), Failure> {
        tried.borrow_mut().push(bundle.to_owned());
        if bundle == damaged {
            Err(Failure::Check(CheckError::Unchecked(
                "OSStatus -67054".into(),
            )))
        } else {
            Ok(())
        }
    };
    let now = Cell::new(Duration::ZERO);
    let mut looks = Looks::new(bounds(RUNNING));
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
    assert!(matches!(
        asked,
        Some((ref bundle, Round::Failed { .. })) if *bundle == damaged
    ));
    for _ in 0..3 {
        now.set(now.get() + LONGEST_RETRY);
        let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
        let (bundle, round) = asked.expect("an asker is tried once the backoff ends");
        assert_eq!(bundle, good, "the uid's next, which never failed");
        assert!(matches!(round, Round::Applied));
    }
    let damaged_asker = asker(501, &damaged);
    assert!(
        askers.now().contains(&damaged_asker),
        "the damaged copy is not forgotten"
    );
    let alone = asking(&[damaged_asker]);
    now.set(now.get() + LONGEST_RETRY);
    let (_, asked) = looks.look(|| now.get(), false, &source, &image, &alone, adopt);
    assert_eq!(
        asked.map(|(bundle, _)| bundle),
        Some(damaged.clone()),
        "alone, it is tried again"
    );
    assert_eq!(
        *tried.borrow(),
        vec![damaged.clone(), good.clone(), good.clone(), good, damaged]
    );
}

/// The adoption loop gives each asker it authenticated its build, read from that bundle's own
/// sealed plist on the adoption thread (the server reads nothing of it), and that build decides
/// which of a uid's entries goes when it asks again: the lower one, though it asked later.
/// NEGATIVE CONTROL (run): with no build given, the oldest ask goes, the newer build's.
#[test]
fn the_adoption_loop_gives_each_authenticated_asker_its_build() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (newer, older, third) = (
        root.join("newer.app"),
        root.join("older.app"),
        root.join("third.app"),
    );
    release(&newer, RUNNING + 30, 1);
    release(&older, RUNNING + 20, 2);
    release(&third, RUNNING + 10, 3);
    let askers = asking(&[asker(501, &older), asker(501, &newer)]);
    let adopt = |_: &Path| -> Result<(), Failure> {
        Err(Failure::Check(CheckError::Unchecked(
            "OSStatus -67054".into(),
        )))
    };
    let mut looks = Looks::new(bounds(RUNNING));
    let (_, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &askers, adopt);
    assert!(asked.is_some());
    let again = asker(501, &third);
    assert!(askers.asked(again.uid, again.cdhash, &executable_of(&third)));
    let kept: Vec<PathBuf> = askers.now().into_iter().map(|asker| asker.bundle).collect();
    assert_eq!(kept, vec![third, newer]);
}

/// A failure is reported (recorded in its bundle's own file) the first time it is said of that
/// bundle: two bundles of one uid that fail alike are each recorded, and one that fails alike
/// again is not. NEGATIVE CONTROL (run): with what was said kept per uid, the second bundle's
/// failure is never recorded.
#[test]
fn a_failure_is_reported_once_for_each_bundle_of_a_uid() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let (one, two) = (root.join("one.app"), root.join("two.app"));
    release(&one, RUNNING + 30, 1);
    release(&two, RUNNING + 25, 2);
    let askers = asking(&[asker(501, &one), asker(501, &two)]);
    let adopt = |_: &Path| -> Result<(), Failure> {
        Err(Failure::Check(CheckError::Unchecked(
            "OSStatus -67054".into(),
        )))
    };
    let now = Cell::new(Duration::ZERO);
    let mut looks = Looks::new(bounds(RUNNING));
    let mut said = Vec::new();
    for _ in 0..3 {
        let (_, asked) = looks.look(|| now.get(), false, &source, &image, &askers, adopt);
        let Some((bundle, Round::Failed { report, .. })) = asked else {
            panic!("each look tries an asker");
        };
        said.push((bundle, report));
        now.set(now.get() + LONGEST_RETRY);
    }
    assert_eq!(said, vec![(one.clone(), true), (two, true), (one, false)]);
}

/// While an adoption check whose bound ended still runs, no look clones anything: the next asker
/// is not cloned and checked for a check the gate would turn away, and is not then made to wait
/// out that check's bound; once the check ends it is tried at the next look. The broker's loop
/// says whether one runs from the adoption's own gate. NEGATIVE CONTROL (run): with the running
/// check not read, the innocent asker is cloned while it runs.
#[test]
fn no_look_clones_while_an_overdue_adoption_check_runs() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("gone.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    let next = root.join("next.app");
    release(&next, RUNNING + 10, 1);
    let askers = asking(&[asker(501, &next)]);
    let tried = Cell::new(0);
    let adopt = |_: &Path| -> Result<(), Failure> {
        tried.set(tried.get() + 1);
        Ok(())
    };
    let mut looks = Looks::new(bounds(RUNNING));
    let (round, asked) = looks.look(|| Duration::ZERO, true, &source, &image, &askers, adopt);
    assert!(matches!(round, Round::Quiet) && asked.is_none());
    assert_eq!(tried.get(), 0, "nothing is cloned while a check runs");
    let (_, asked) = looks.look(|| Duration::ZERO, false, &source, &image, &askers, adopt);
    assert!(matches!(asked, Some((_, Round::Applied))));
    assert_eq!(tried.get(), 1);
}

/// A source whose clone passed its bound settles until its executable changes, as a refusal of
/// its code does: root does not clone, count and remove a tree its account chose the size of at
/// every retry. NEGATIVE CONTROL (run): with the size refusal left unsettled, the source is
/// cloned again once its backoff ends.
#[test]
fn a_source_past_the_bound_settles_until_it_changes() {
    let (_root, root) = scratch();
    let (source, image) = (root.join("source.app"), root.join("lan").join("aterm.app"));
    release(&image, RUNNING, 0);
    release(&source, RUNNING + 1, 1);
    let mut adoption = Adoption::default();
    let first = adoption.round(
        || Duration::ZERO,
        &source,
        &image,
        RUNNING,
        || {
            Err(Failure::TooLarge(
                "its clone holds more than 10 entries".into(),
            ))
        },
    );
    assert!(matches!(
        first,
        Round::Failed {
            error: Failure::TooLarge(_),
            report: true
        }
    ));
    let later = adoption.round(
        || LONGEST_RETRY * 4,
        &source,
        &image,
        RUNNING,
        || panic!("a source past its bound is not cloned again unchanged"),
    );
    assert!(matches!(later, Round::Quiet));
    release(&source, RUNNING + 2, 2);
    let changed = adoption.round(|| LONGEST_RETRY * 4, &source, &image, RUNNING, || Ok(()));
    assert!(
        matches!(changed, Round::Applied),
        "a changed source is tried"
    );
}

/// The walk of a tree reads only that tree, on its own volume: a link on the way to its root is
/// refused before anything is read, a link inside is counted and never followed (a folder
/// swapped for a link included), a mount at or under the root is refused from the mount table
/// alone, and a tree past [`DEPTH_MOST`] folders deep is refused. Each regular file counts its
/// length. NEGATIVE CONTROLS (run): following a link inside, the walk counts the folder it leads
/// to; with the mount table not read, a mount under the root is walked.
#[test]
fn a_trees_walk_follows_no_link_and_leaves_its_volume_for_none() {
    use aterm_sysprobe::volumes::{Mount, Table};
    let (_root, root) = scratch();
    let tree = root.join("tree.app");
    std::fs::create_dir_all(tree.join("Contents/MacOS")).unwrap();
    std::fs::write(tree.join("Contents/MacOS/aterm"), b"12345").unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    for n in 0..50 {
        std::fs::write(outside.join(n.to_string()), b"x").unwrap();
    }
    std::os::unix::fs::symlink(&outside, tree.join("Contents/Resources")).unwrap();
    let roomy = Tree {
        entries: 1000,
        bytes: u64::MAX,
    };
    let none = Table {
        mounts: Vec::new(),
        complete: true,
        firmlinks: Vec::new(),
    };
    assert_eq!(
        tree_within(&tree, roomy, &none).unwrap(),
        Some(Tree {
            entries: 4,
            bytes: 5,
        }),
        "the link is one entry, never followed"
    );
    let through = root.join("through");
    std::os::unix::fs::symlink(&tree, &through).unwrap();
    assert!(
        tree_within(&through, roomy, &none).is_err(),
        "a link to the root"
    );
    let mounted = Table {
        mounts: vec![Mount {
            on: tree.join("Contents/Share"),
            from: "//server/share".to_owned(),
            fstype: "smbfs".to_owned(),
            local: false,
            automounted: false,
            removable: false,
            read_only: false,
            container: None,
        }],
        complete: true,
        firmlinks: Vec::new(),
    };
    assert!(
        tree_within(&tree, roomy, &mounted).is_err(),
        "a mount under the root"
    );
    let deep = root.join("deep.app");
    let mut at = deep.clone();
    for _ in 0..=DEPTH_MOST {
        at = at.join("d");
    }
    std::fs::create_dir_all(&at).unwrap();
    assert!(tree_within(&deep, roomy, &none).is_err(), "too deep");
}

/// An asker's failure is recorded in its bundle's own file and never takes the place of the
/// source lane's record: after the source's definite refusal and an asker's failure, the
/// source's account still reads its own record, definite, so its expectation lapses at the
/// short bound with its reason, and the asker's account reads the asker's. The askers' records
/// are bounded. NEGATIVE CONTROL (run): with an asker's record written to the source lane's file,
/// the source's account finds no record of its own.
#[test]
fn an_askers_failure_never_takes_the_source_lanes_record() {
    use aterm_uds::lan::adoption::{self, Kind};
    /// Root's records as the running broker's paths name them, read from this test's folder.
    struct Records<'a> {
        source: &'a Path,
        askers: &'a Path,
    }
    impl adoption::Expecting for Records<'_> {
        fn now(&self) -> u64 {
            unreachable!("only the records are read")
        }
        fn record(&self) -> Option<io::Result<Vec<u8>>> {
            unreachable!("only the records are read")
        }
        fn keep(&self, _: &[u8]) -> io::Result<()> {
            unreachable!("only the records are read")
        }
        fn hello(&self) -> Option<aterm_uds::lan::BrokerHello> {
            unreachable!("only the records are read")
        }
        fn refusal(&self, path: &Path) -> io::Result<Vec<u8>> {
            if path == Path::new(aterm_uds::lan::ADOPTION_REFUSED) {
                return std::fs::read(self.source);
            }
            assert_eq!(
                path.parent(),
                Some(Path::new(aterm_uds::lan::ADOPTION_REFUSED_ASKERS))
            );
            std::fs::read(self.askers.join(path.file_name().unwrap()))
        }
        fn journal(&self, _: &str) {}
    }
    let (_root, root) = scratch();
    let (record, askers) = (
        root.join("lan-adoption.refused"),
        root.join("lan-adoption.refused.d"),
    );
    let asking = AskerRefusals { folder: &askers };
    let reader = Records {
        source: &record,
        askers: &askers,
    };
    let (source, asked) = (root.join("source.app"), root.join("asked.app"));
    Refusals {
        path: &record,
        source: &source,
    }
    .after(&Round::Failed {
        error: Failure::Check(CheckError::Refused("not the team".into())),
        report: true,
    });
    asking.after(
        &asked,
        &Round::Failed {
            error: Failure::Check(CheckError::Unanswered("trustd did not answer".into())),
            report: true,
        },
    );
    let ours = adoption::refusals_of(&reader, &source);
    assert_eq!(ours.len(), 1, "{ours:?}");
    assert_eq!(ours[0].kind, Kind::Definite);
    assert!(ours[0].reason.contains("not the team"), "{ours:?}");
    assert_eq!(adoption::bound(ours.first()), adoption::LAPSE);
    let theirs = adoption::refusals_of(&reader, &asked);
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert_eq!(theirs[0].kind, Kind::NoVerdict);
    let mode = std::fs::metadata(&askers).unwrap().mode() & 0o777;
    assert_eq!(mode, 0o755, "{mode:o}");

    for n in 0..crate::askers::MOST + 2 {
        asking.after(
            &root.join(format!("{n}.app")),
            &Round::Failed {
                error: Failure::NotNewer("its build 1 is not newer".into()),
                report: true,
            },
        );
    }
    assert_eq!(
        std::fs::read_dir(&askers).unwrap().count(),
        crate::askers::MOST
    );
    assert_eq!(
        adoption::refusals_of(&reader, &source),
        ours,
        "the source's stands"
    );

    let last = root.join(format!("{}.app", crate::askers::MOST + 1));
    let kept = std::fs::read_dir(&askers).unwrap().count();
    asking.after(&last, &Round::Applied);
    assert!(
        !aterm_uds::lan::adoption::asker_refusal_in(&askers, &last).exists(),
        "an adoption clears its bundle's own record"
    );
    assert_eq!(std::fs::read_dir(&askers).unwrap().count(), kept - 1);
    assert!(record.exists(), "and never the source lane's");
}
