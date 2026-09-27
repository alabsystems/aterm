// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The claimant census end to end: real bundles on disk, signed ad-hoc by the
//! real `codesign`, read through the shipping lister and candidate reader.
//!
//! The table tests in `consent.rs` drive the classification with injected
//! reads. This one exercises the reads themselves — including `codesign`'s
//! commented `# designated =>` form, which the injected tests cannot see.

#![cfg(target_os = "macos")]

use std::path::Path;

use aterm_containment::DrClass;
use aterm_containment::consent::{
    Candidate, Claimant, Enumeration, classify_claimants, list_root, read_candidate, still_claims,
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
    let status = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(root.join(name))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    assert!(
        matches!(status, Ok(s) if s.success()),
        "codesign --sign - failed for {name}: {status:?}"
    );
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
    };
    assert!(still_claims(&copy, ID), "the same unsigned copy");

    // An unreadable executable makes `codesign` answer `Permission denied`: a
    // read that failed, with no requirement text — which a text-only
    // comparison would have taken for the unsigned copy. (Root reads it anyway.)
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
