// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The claimant census end to end: real bundles on disk, signed ad-hoc by the
//! real `codesign`, read through the shipping lister and candidate reader.
//!
//! The table tests in `consent.rs` drive the classification with injected
//! reads. This one exercises the reads themselves — including `codesign`'s
//! commented `# designated =>` form, which the injected tests cannot see.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use aterm_containment::DrClass;
use aterm_containment::consent::{
    BundleIdentity, Candidate, Claimant, Enumeration, Retired, classify_claimants, list_root,
    locate_candidate, read_candidate, retire_claimants, still_claims,
};

const ID: &str = "com.test.aterm-census";

/// Lay `<root>/<name>` as a bundle claiming [`ID`] whose executable prints
/// `marker` (so each gets its own cdhash), and sign it ad-hoc.
fn lay_signed(root: &Path, name: &str, marker: &str) {
    let contents = root.join(name).join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).expect("layout");
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>CFBundleIdentifier</key><string>{ID}</string>\n\
             <key>CFBundleExecutable</key><string>aterm</string>\n\
             </dict></plist>\n"
        ),
    )
    .expect("plist");
    std::fs::write(
        contents.join("MacOS/aterm"),
        format!("#!/bin/sh\necho {marker}\n"),
    )
    .expect("exe");
    aterm_uds::deadline::sign_ad_hoc(&root.join(name));
}

#[test]
fn the_real_census_tells_a_rollback_and_a_backup_from_the_running_copy() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("resolve tempdir");
    lay_signed(&root, "aterm.app", "LIVE");
    lay_signed(&root, "aterm.app.rollback", "ROLLBACK");
    lay_signed(&root, "aterm-backup.app", "BACKUP");
    std::fs::create_dir_all(root.join("Other.app/Contents")).expect("another program");

    let running = root.join("aterm.app");
    let census = classify_claimants(
        Some(&running),
        &[(root.clone(), true)],
        Some(&[]),
        list_root,
        locate_candidate,
        |candidate| read_candidate(candidate, ID, &[]),
    );

    assert_eq!(census.enumeration, Enumeration::Complete);
    assert_eq!(census.found.len(), 3, "{:?}", census.found);
    assert!(census.found[0].running && census.found[0].path == running);
    assert!(
        census
            .found
            .iter()
            .all(|c| c.dr == DrClass::Cdhash && c.signing == "adhoc"),
        "an ad-hoc signature's commented requirement reads as cdhash: {:?}",
        census.found
    );
    assert_eq!(
        census.conflicting().len(),
        2,
        "different cdhashes are different identities: {:?}",
        census.found
    );
    assert!(
        census.retirable().is_empty(),
        "beside an unstable running copy nothing is retirable"
    );
}

/// The Trash's last check before each move, on real bundles: `still_claims`
/// holds for the copy the census read, and fails once that copy is re-signed
/// (a new cdhash), when asked about another id, and once the copy is gone.
#[test]
fn still_claims_follows_the_bundle_on_disk() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("resolve tempdir");
    lay_signed(&root, "aterm.app.rollback", "ROLLBACK");
    let path = root.join("aterm.app.rollback");
    let Candidate::Ours(id) = read_candidate(&path, ID, &[]) else {
        panic!("the laid bundle claims {ID}");
    };
    let copy = Claimant {
        path: path.clone(),
        dr: id.dr,
        dr_text: id.dr_text,
        signing: id.signing,
        team: id.team,
        running: false,
        links: Vec::new(),
    };
    assert!(still_claims(&copy, ID), "the copy the census read");
    assert!(!still_claims(&copy, "com.test.someone-else"), "another id");

    lay_signed(&root, "aterm.app.rollback", "RESIGNED");
    assert!(!still_claims(&copy, ID), "re-signed: a different identity");

    std::fs::remove_dir_all(&path).expect("remove");
    assert!(!still_claims(&copy, ID), "gone");
}

/// An unsigned copy is told apart from a read that failed: both have no
/// requirement text, but the failed read has no class and is never the copy.
#[test]
fn still_claims_tells_an_unsigned_copy_from_a_failed_read() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("resolve tempdir");
    let path = root.join("aterm-unsigned.app");
    let contents = path.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).expect("layout");
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{ID}</string>\
             <key>CFBundleExecutable</key><string>aterm</string></dict></plist>"
        ),
    )
    .expect("plist");
    std::fs::write(contents.join("MacOS/aterm"), "#!/bin/sh\necho UNSIGNED\n").expect("exe");
    let Candidate::Ours(id) = read_candidate(&path, ID, &[]) else {
        panic!("the unsigned bundle claims {ID}");
    };
    assert_eq!(id.dr, DrClass::Unsigned, "{id:?}");
    assert!(id.dr_text.is_empty());
    let copy = Claimant {
        path: path.clone(),
        dr: id.dr,
        dr_text: id.dr_text,
        signing: id.signing,
        team: id.team,
        running: false,
        links: Vec::new(),
    };
    assert!(still_claims(&copy, ID), "the same unsigned copy");

    // An unreadable executable makes `codesign` answer `Permission denied`: a
    // failed read with no requirement text, not an unsigned copy. (Root reads
    // it anyway.)
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    let exe = contents.join("MacOS/aterm");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o000)).expect("lock exe");
    let failed_read_matches = still_claims(&copy, ID);
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("unlock exe");
    assert!(
        !failed_read_matches,
        "codesign could not read it: no class, so not the copy"
    );
}

/// One bundle is one claimant however many names reach it. Beside a running
/// copy (presented as Developer ID, however it is spelled) sit an ad-hoc
/// rollback, an ad-hoc dev build `LaunchServices` registered, a link in the
/// app folder to that dev build, and a link in a second app folder to the
/// running copy. The census finds three bundles, not five: the link to the
/// running copy is the running copy, and the link to the dev build is that
/// build. The rollback is offered for the Trash; the dev build a link leads
/// to is listed and never offered, and a plan naming the link moves nothing.
#[test]
fn a_link_is_the_bundle_it_names() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).expect("resolve tempdir");
    let (apps, home_apps) = (base.join("Applications"), base.join("HomeApplications"));
    let build = base.join("dev/target/release/bundle");
    for dir in [&apps, &home_apps, &build] {
        std::fs::create_dir_all(dir).expect("folder");
    }
    lay_signed(&apps, "aterm.app", "LIVE");
    lay_signed(&apps, "aterm.app.rollback", "ROLLBACK");
    lay_signed(&build, "aterm.app", "DEV");
    let (running, rollback, dev) = (
        apps.join("aterm.app"),
        apps.join("aterm.app.rollback"),
        build.join("aterm.app"),
    );
    let (dev_link, running_link) = (apps.join("aterm-dev.app"), home_apps.join("aterm.app"));
    std::os::unix::fs::symlink(&dev, &dev_link).expect("link to the dev build");
    std::os::unix::fs::symlink(&running, &running_link).expect("link to the running copy");

    let census = classify_claimants(
        Some(&running),
        &[(apps.clone(), true), (home_apps, false)],
        Some(std::slice::from_ref(&dev)),
        list_root,
        locate_candidate,
        |candidate| {
            if std::fs::canonicalize(candidate).is_ok_and(|real| real == running) {
                Candidate::Ours(BundleIdentity {
                    bundle_id: Some(ID.to_string()),
                    dr_text: "designated => identifier \"x\" and anchor apple generic and \
                              certificate leaf[subject.OU] = \"T\""
                        .to_string(),
                    dr: DrClass::Identity,
                    signing: "developer-id",
                    team: Some("T".to_string()),
                })
            } else {
                read_candidate(candidate, ID, &[])
            }
        },
    );
    let found: Vec<(&Path, bool, &[PathBuf])> = census
        .found
        .iter()
        .map(|c| (c.path.as_path(), c.running, c.links.as_slice()))
        .collect();
    assert_eq!(
        found,
        vec![
            (running.as_path(), true, std::slice::from_ref(&running_link)),
            (rollback.as_path(), false, &[][..]),
            (dev.as_path(), false, std::slice::from_ref(&dev_link)),
        ]
    );
    assert_eq!(census.enumeration, Enumeration::Complete);
    let offered: Vec<&Path> = census
        .offered_for_trash(true)
        .iter()
        .map(|c| c.path.as_path())
        .collect();
    assert_eq!(
        offered,
        vec![rollback.as_path()],
        "the dev build a link leads to is never offered"
    );
    let moved = std::cell::RefCell::new(Vec::new());
    let report = retire_claimants(
        &[dev_link.clone(), dev.clone(), rollback.clone()],
        &census,
        |_| true,
        |_| Some(false),
        |path| {
            moved.borrow_mut().push(path.to_path_buf());
            Ok(())
        },
    );
    assert_eq!(
        report,
        vec![
            (dev_link, Retired::NotOffered),
            (dev, Retired::NotOffered),
            (rollback.clone(), Retired::Moved)
        ]
    );
    assert_eq!(*moved.borrow(), vec![rollback]);
}
