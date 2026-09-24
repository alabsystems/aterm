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
use aterm_containment::consent::{Enumeration, classify_claimants, list_root, read_candidate};

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
