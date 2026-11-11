// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn census_ranks_destinations_and_reports_incomplete_delivery() {
    let mut census = Census::default();
    census.add("/owned/logs/a", 0);
    census.add("/owned/logs/b", 0);
    census.add("/owned/state/c", 1);
    // Negative control: a lexical-prefix match must not attribute a neighbour.
    census.add("/owned-other/logs/d", 2);
    assert_eq!(
        census.ranked(&["/owned".into()]),
        vec![("/owned/logs", 2), ("/owned/state", 1)]
    );
    assert_eq!(census.dropped, 2);
    for i in 0..16_384 {
        census.add(&format!("/many/{i}/file"), 0);
    }
    assert_eq!(census.counts.len(), 16_384);
    assert_eq!(census.overflow, 3);
    census.add("/owned/logs/a", 0);
    assert_eq!(census.ranked(&["/owned".into()])[0].1, 3);
}

#[test]
fn focused_files_distinguish_journals_and_keys_without_hiding_global_drops() {
    let mut files = Census::configured(&["/owned/state".into()], true).unwrap();
    for path in ["/owned/state/control.astream", "/owned/state/device.age"] {
        files.add(path, 0);
    }
    files.add("/owned/state-neighbour/other", 2);
    assert_eq!(
        files.ranked(&[]),
        vec![
            ("/owned/state/control.astream", 1),
            ("/owned/state/device.age", 1)
        ]
    );
    assert_eq!(files.dropped, 1);
    // Negative control: the broad directory instrument deliberately coalesces these.
    let mut directories = Census::default();
    directories.add("/owned/state/control.astream", 0);
    directories.add("/owned/state/device.age", 0);
    assert_eq!(directories.ranked(&[]), vec![("/owned/state", 2)]);
    assert!(Census::configured(&[], true).is_err());
    assert!(Census::configured(&["relative".into()], true).is_err());
    assert!(Census::configured(&vec!["/root".into(); 257], true).is_err());
    assert!(Census::configured(&[format!("/{}", "x".repeat(65_536)).into()], true).is_err());
}

#[test]
fn invalid_capture_never_announces_a_ready_subscription() {
    assert!(
        capture(Duration::ZERO, Granularity::Directories(&[]), || panic!(
            "not started"
        ))
        .is_err()
    );
    assert!(
        capture(Duration::from_secs(1), Granularity::Files(&[]), || panic!(
            "not started"
        ))
        .is_err()
    );
}
