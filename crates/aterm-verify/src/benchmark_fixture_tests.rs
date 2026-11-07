// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic object identity across independent benchmark processes.

use std::path::Path;
use std::time::Duration;

#[expect(
    dead_code,
    reason = "shared benchmark support also contains inventory and phase probes"
)]
#[path = "../benches/support/journal_fixture.rs"]
mod fixture;

/// Fixed fixture inputs create the same child under different process timezones.
/// The raw Git control must differ, so a helper using wall time cannot pass.
#[test]
fn child_objects_ignore_process_timezone() {
    const REPO: &str = "ATERM_BENCH_FIXTURE_CHILD_REPO";
    const PARENT: &str = "ATERM_BENCH_FIXTURE_CHILD_PARENT";
    const CASE: &str = "benchmark_fixture_tests::child_objects_ignore_process_timezone";
    if let Some(repo) = std::env::var_os(REPO) {
        let repo = Path::new(&repo);
        let parent = std::env::var(PARENT).unwrap();
        let fixed = fixture::same_tree_child(repo, &parent).unwrap();
        let tree = fixture::git(repo, &["rev-parse", &format!("{parent}^{{tree}}")]).unwrap();
        let mut raw = aterm_uds::fixture_git::git();
        raw.current_dir(repo).args([
            "commit-tree",
            &tree,
            "-p",
            &parent,
            "-m",
            "unpinned timestamp control",
        ]);
        let output = aterm_uds::deadline::output_within(&mut raw, Duration::from_secs(60)).unwrap();
        assert!(output.status.success());
        let unpinned = String::from_utf8(output.stdout).unwrap();
        println!("fixture-fixed:{fixed}");
        println!("fixture-unpinned:{}", unpinned.trim());
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path();
    fixture::git(repo, &["init", "--quiet", "-b", "main"]).unwrap();
    fixture::git(repo, &["config", "user.name", "fixture"]).unwrap();
    fixture::git(repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fixture::git(repo, &["commit", "--quiet", "--allow-empty", "-m", "root"]).unwrap();
    let parent = fixture::git(repo, &["rev-parse", "HEAD"]).unwrap();
    let mut fixed = Vec::new();
    let mut unpinned = Vec::new();
    for timezone in ["UTC0", "UTC-11"] {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", CASE, "--nocapture", "--test-threads=1"])
            .env(REPO, repo)
            .env(PARENT, &parent)
            .env("TZ", timezone);
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let value = |marker: &str| {
            let oid = stdout
                .lines()
                .find_map(|line| line.split_once(marker).map(|(_, oid)| oid.trim()))
                .expect("the exact owned child wrote its object identity");
            assert!(!oid.is_empty() && oid.bytes().all(|b| b.is_ascii_hexdigit()));
            oid.to_owned()
        };
        fixed.push(value("fixture-fixed:"));
        unpinned.push(value("fixture-unpinned:"));
    }
    assert_ne!(
        unpinned[0], unpinned[1],
        "the timezone control must affect Git objects"
    );
    assert_eq!(
        fixed[0], fixed[1],
        "fixture bytes must ignore process timezone"
    );
}
