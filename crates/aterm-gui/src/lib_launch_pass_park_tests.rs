// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const WALK: i64 = aterm_update_core::pkg_check::FULL_PASS_INTERVAL_SECS as i64;

fn store(dir: &std::path::Path, record: &str) -> atpkg::store::Layout {
    let layout = atpkg::store::Layout {
        prefix: dir.join("pkg"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    std::fs::write(layout.status(), record).unwrap();
    layout
}

#[test]
fn the_store_record_decides_on_the_pass_stamps_not_the_last_write() {
    let success = 1_700_000_000;
    assert_eq!(launch_update_park(false, None, success), None);
    let dir = aterm_tempfile::tempdir().unwrap();
    let none = store(
        dir.path(),
        "schema = 1\nupdated_at = \"2023-11-14T22:13:20Z\"\n",
    );
    assert_eq!(
        launch_update_park(false, Some(&none), success + 30),
        None,
        "a write with no success on record: due"
    );
    let checked = store(
        dir.path(),
        "schema = 1\nupdated_at = \"2023-11-14T22:13:20Z\"\n\
         last_success_at = \"2023-11-14T22:13:20Z\"\n",
    );
    assert_eq!(
        launch_update_park(false, Some(&checked), success + 30),
        Some(Duration::from_secs((WALK - 30) as u64))
    );
    assert_eq!(
        launch_update_park(false, Some(&checked), success + WALK),
        None
    );
    let door = store(
        dir.path(),
        "schema = 1\nupdated_at = \"2023-11-14T22:13:50Z\"\n\
         last_success_at = \"2023-11-14T22:13:20Z\"\n",
    );
    assert_eq!(
        launch_update_park(false, Some(&door), success + 60),
        Some(Duration::from_secs((WALK - 60) as u64)),
        "a later write is no failed pass"
    );
    let failed = store(
        dir.path(),
        "schema = 1\nupdated_at = \"2023-11-14T22:13:50Z\"\n\
         last_success_at = \"2023-11-14T22:13:20Z\"\nlast_pass = \"failed\"\n\
         last_pass_at = \"2023-11-14T22:13:50Z\"\n",
    );
    assert_eq!(
        launch_update_park(false, Some(&failed), success + 60),
        None,
        "the control: a failure after the success is due now"
    );
}

#[test]
fn an_update_successor_runs_its_pass_now_whatever_the_record_says() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = store(
        dir.path(),
        "schema = 1\nupdated_at = \"2023-11-14T22:13:20Z\"\nenabled = true\n\
         last_success_at = \"2023-11-14T22:13:20Z\"\n",
    );
    assert_eq!(
        launch_update_park(false, Some(&layout), 1_700_000_030),
        Some(Duration::from_secs((WALK - 30) as u64)),
        "an ordinary launch parks for the remainder of the interval"
    );
    assert_eq!(
        launch_update_park(true, Some(&layout), 1_700_000_030),
        None,
        "an update's successor updates at once: its predecessor passed on the old build"
    );
    assert_eq!(launch_update_park(true, None, 1_700_000_030), None);
    assert_eq!(launch_update_park(false, None, 1_700_000_030), None);
}
