// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

fn bundle(at: &Path, bytes: &[u8]) {
    let exe = executable_of(at);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, bytes).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o777)).unwrap();
}

#[test]
fn a_passing_clone_is_swapped_in_and_a_refused_one_leaves_the_image() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (source, image) = (
        root.path().join("source.app"),
        root.path().join("lan").join("aterm.app"),
    );
    bundle(&source, b"new");
    bundle(&image, b"old");
    let refused = adopt(&source, &image, |_| {
        Err(CheckError::Refused("not the team".to_owned()))
    })
    .unwrap_err();
    assert!(matches!(refused, Failure::Check(CheckError::Refused(_))));
    assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"old");
    let mut judged = None;
    adopt(&source, &image, |staged| {
        judged = Some(staged.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"new");
    let judged = judged.unwrap();
    assert_ne!(
        judged, source,
        "the clone is judged, never the person's bundle"
    );
    assert!(!judged.exists(), "the replaced image is gone");
    let mode = std::fs::metadata(executable_of(&image)).unwrap().mode();
    assert_eq!(mode & 0o022, 0, "no group or other write bit: {mode:o}");
    let entries: Vec<_> = std::fs::read_dir(image.parent().unwrap())
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1, "no staging left beside the image");
    assert_eq!(std::fs::read(executable_of(&source)).unwrap(), b"new");

    // A temporary staging obstruction is ordinary filesystem failure, not
    // a code refusal or a new source generation. Its repair must be noticed
    // by the next ordinary watch round without restarting the broker.
    bundle(&image, b"old");
    let image_exe = executable_of(&image);
    let original = stamp(&source).unwrap();
    let staging_parent = root.path().join("unavailable-staging-parent");
    std::fs::write(&staging_parent, b"temporary obstruction").unwrap();
    let mut observation = Adoption::default();
    let now = std::cell::Cell::new(Duration::ZERO);
    let first = observation.round(
        || now.get(),
        &source,
        &image,
        || {
            adopt_allocating(
                &source,
                &image,
                |_| panic!("failed private allocation precedes code verification"),
                |_| aterm_tempfile::TempDir::new_in(&staging_parent),
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
    assert_eq!(std::fs::read(&image_exe).unwrap(), b"old");
    std::fs::remove_file(&staging_parent).unwrap();
    std::fs::create_dir(&staging_parent).unwrap();
    now.set(LOOK_EVERY);
    let retried = observation.round(
        || now.get(),
        &source,
        &image,
        || {
            adopt_allocating(
                &source,
                &image,
                |_| Ok(()),
                |_| aterm_tempfile::TempDir::new_in(&staging_parent),
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
        b"new",
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
    std::fs::write(source.join("Contents/Info.plist"), b"<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>aterm</string><key>CFBundleIdentifier</key><string>com.aterm.adoption-fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>").unwrap();
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
        || {
            adopt(&source, &image, |candidate| {
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
    assert_eq!(std::fs::read(&image_exe).unwrap(), b"new");
    std::fs::remove_file(&resource).unwrap();
    std::os::unix::fs::symlink("../MacOS/aterm", &resource).unwrap();
    assert_eq!(stamp(&source).unwrap(), signed_stamp);
    now.set(now.get() + LOOK_EVERY);
    let repaired = observation.round(
        || now.get(),
        &source,
        &image,
        || {
            adopt(&source, &image, |candidate| {
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
        || {
            adopt(&source, &image, |_| {
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
                || panic!("unchanged refused source must not clone or reverify")
            ),
            Round::Quiet
        ));
    }
    bundle(&source, b"retry");
    let last_attempt = std::cell::Cell::new(None);
    let intervals = std::cell::RefCell::new(Vec::new());
    let mut reports = 0;
    for _ in 0..600 {
        let result = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt(&source, &image, |_| {
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
        || panic!("the same retry is not due"),
    );
    assert!(matches!(pending, Round::Quiet));
    bundle(&source, b"a new source bypasses the old backoff");
    assert!(matches!(
        observation.round(
            || now.get(),
            &source,
            &image,
            || adopt(&source, &image, |_| Ok(()))
        ),
        Round::Applied
    ));
    assert_eq!(
        std::fs::read(&image_exe).unwrap(),
        b"a new source bypasses the old backoff"
    );

    bundle(&source, b"changing");
    let changed = observation.round(
        || now.get(),
        &source,
        &image,
        || {
            adopt(&source, &image, |_| {
                bundle(&source, b"after the check started");
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
            || adopt(&source, &image, |_| Ok(()))
        ),
        Round::Applied
    ));
}

#[test]
fn a_source_that_is_a_link_or_holds_one_out_of_the_bundle_is_never_adopted() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (real, link, image) = (
        root.path().join("real.app"),
        root.path().join("link.app"),
        root.path().join("lan").join("aterm.app"),
    );
    bundle(&real, b"new");
    bundle(&image, b"old");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut judged = false;
    adopt(&link, &image, |_| {
        judged = true;
        Ok(())
    })
    .unwrap_err();
    assert!(!judged, "a link is refused before anything is judged");
    assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"old");
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
    adopt(&real, &image, |_| Ok(())).unwrap();
    assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"new");

    for out in ["../../../outside", "/Users//someone/evil", "../x/../../.."] {
        let escape = resources.join("escape");
        let _ = std::fs::remove_file(&escape);
        std::os::unix::fs::symlink(out, &escape).unwrap();
        bundle(&real, b"newer");
        let refused = adopt(&real, &image, |_| Ok(())).unwrap_err();
        assert!(matches!(refused, Failure::Refused(_)), "{out}");
        assert_eq!(
            std::fs::read(executable_of(&image)).unwrap(),
            b"new",
            "{out}"
        );
    }
    let entries: Vec<_> = std::fs::read_dir(image.parent().unwrap())
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1, "no staging left beside the image");
}

#[test]
fn a_first_adoption_renames_the_clone_into_place() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (source, image) = (
        root.path().join("source.app"),
        root.path().join("aterm.app"),
    );
    bundle(&source, b"first");
    adopt(&source, &image, |_| Ok(())).unwrap();
    assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"first");
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
            || panic!("an incomplete source must not be cloned or checked")
        ),
        Round::Failed {
            error: Failure::Io(_),
            ..
        }
    ));
}
