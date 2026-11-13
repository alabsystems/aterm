// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every rostered harness is a subject, named once; Muse is played; the windows hold the
/// harness's own clocks.
#[test]
fn the_subjects_cover_the_roster_and_size_their_windows_from_the_harness() {
    for row in aterm_agent::harness::contract::ROSTER {
        assert!(subject(row.id).is_some(), "{} is rostered", row.id);
    }
    let ids: Vec<&str> = SUBJECTS.iter().map(|s| s.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "{ids:?}");
    assert!(subject("codex-alab").is_none(), "codex-alab proves itself");
    let muse = subject("muse-alab")
        .and_then(|s| s.play.as_ref())
        .expect("muse plays");
    let [a, b] = muse.builds;
    let (ia, ib) = (
        (muse.build_id)(a).expect("a build id"),
        (muse.build_id)(b).expect("a build id"),
    );
    assert!(ib > ia, "the second build is newer by atpkg's order");
    assert!(still_window(muse) > muse.grace + muse.settle);
    let every = typing_every(muse);
    assert!(every < muse.grace && every > muse.settle, "{every:?}");
    assert!(
        muse.timing
            .1
            .split(',')
            .nth(2)
            .and_then(|g| g.parse::<u64>().ok())
            == Some(u64::try_from(muse.grace.as_millis()).unwrap_or(0)),
        "the grace is the one the seam sets"
    );
}

#[test]
fn a_harness_repos_version_is_its_workspace_package_version() {
    assert_eq!(
        package_version("[workspace.package]\nversion = \"0.1.0\"\n[package]\nname = \"x\"\nversion.workspace = true\n")
            .as_deref(),
        Some("0.1.0")
    );
    assert_eq!(
        package_version("[package]\nname = \"x\"\nversion = \"0.2.0\"\n").as_deref(),
        Some("0.2.0")
    );
    assert_eq!(package_version("[package]\nname = \"x\"\n"), None);
    assert_eq!(package_version("not toml ["), None);
}
