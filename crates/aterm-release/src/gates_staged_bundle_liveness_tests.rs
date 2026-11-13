// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::bundle::RunningProcess;

const DIST: &str = "/Users//a/aterm/dist";

fn procs(rows: &[(&str, &str)]) -> Vec<RunningProcess> {
    rows.iter()
        .map(|(pid, comm)| ((*pid).to_string(), (*comm).to_string()))
        .collect()
}

#[test]
fn a_live_staging_bundle_refuses_naming_the_pid_and_the_path() {
    let dist = Path::new(DIST);
    let listed = procs(&[
        (
            "7",
            "/Users//a/aterm/dist/cut-1790000000.noindex/aterm.app/Contents/MacOS/aterm",
        ),
        ("4242", "/Applications/aterm.app/Contents/MacOS/aterm"),
    ]);
    let error = staged_bundle_liveness_gate(dist, &|| Some(listed.clone()))
        .expect_err("a live per-claim staging bundle must refuse the cut")
        .to_string();
    assert!(error.contains("pid 7 "), "{error}");
    assert!(
        !error.contains("4242"),
        "an installed copy is not a staging bundle: {error}"
    );
}

#[test]
fn an_unknown_process_list_refuses() {
    staged_bundle_liveness_gate(Path::new(DIST), &|| None)
        .expect_err("\"could not look\" never licenses a delete");
}

#[test]
fn an_answered_look_with_nothing_under_a_staging_dir_passes() {
    let dist = Path::new(DIST);
    assert_eq!(
        staged_bundle_liveness_gate(dist, &|| Some(Vec::new())).unwrap(),
        0
    );
    let listed = procs(&[
        (
            "85619",
            "/Users//a/aterm/dist/cut-app/aterm.app/Contents/MacOS/aterm",
        ),
        ("1", "/Users//a/aterm/dist/aterm.app/Contents/MacOS/aterm"),
        (
            "2",
            "/Users//a/aterm/dist/aterm.app.rollback/Contents/MacOS/aterm",
        ),
        (
            "3",
            "/Users//a/aterm/dist/cut-app-old/aterm.app/Contents/MacOS/aterm",
        ),
        (
            "4",
            "/Users//a/aterm/dist/cut-12x.noindex/aterm.app/Contents/MacOS/aterm",
        ),
        (
            "5",
            "/Users//a/aterm/dist-dev/cut-app/aterm.app/Contents/MacOS/aterm",
        ),
        ("6", "/Applications/aterm.app/Contents/MacOS/aterm"),
    ]);
    assert_eq!(
        staged_bundle_liveness_gate(dist, &|| Some(listed.clone())).unwrap(),
        7
    );
}
